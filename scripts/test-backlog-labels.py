#!/usr/bin/env python3
"""Exercise `backlog-labels.py` with `gh` replaced by a fake `subprocess.run`, and check the issue
templates that apply `triage/pending` (docs/design/issue-triage.md §3, §4): no network, no GitHub."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import re
import shlex
import subprocess
import sys
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent
TEMPLATES = ROOT / '.github' / 'ISSUE_TEMPLATE'


def load():
    spec = importlib.util.spec_from_file_location('backlog_labels', ROOT / 'scripts' / 'backlog-labels.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


labels = load()


class FakeGh:
    """Records every command; answers `gh api repos/O/R/issues` (page by page, like GitHub) and
    `gh repo view` from a fixture of open issues."""

    def __init__(self, issues=(), repo='o/r'):
        self.issues = issues
        self.repo = repo
        self.calls = []

    def __call__(self, command, **kwargs):
        self.calls.append(command)
        stdout = ''
        if command[:2] == ['gh', 'api']:
            params = dict(arg.split('=', 1) for arg in command[command.index('-f') + 1::2] if '=' in arg)
            wanted, per_page, page = params['labels'], int(params['per_page']), int(params['page'])
            rows = [{'number': n, 'labels': [{'name': name} for name in names]}
                    for n, names in self.issues if wanted in names]
            stdout = json.dumps(rows[(page - 1) * per_page:page * per_page])
        elif command[:3] == ['gh', 'repo', 'view']:
            stdout = self.repo + '\n'
        return subprocess.CompletedProcess(command, 0, stdout, '')

    def reads(self):
        return [c for c in self.calls if c[:2] == ['gh', 'api']]

    def writes(self):
        return [c for c in self.calls if c[:2] != ['gh', 'api'] and c[:3] != ['gh', 'repo', 'view']]


def run(fake, *argv):
    out = io.StringIO()
    with mock.patch.object(labels.subprocess, 'run', fake), \
            mock.patch.object(sys, 'argv', ['backlog-labels.py', *argv]), contextlib.redirect_stdout(out):
        labels.main()
    return out.getvalue().splitlines()


ISSUES = [(1, ['Agent UX Improvement', 'bug']), (2, ['test-perf']), (3, ['enhancement', 'class/uix']),
          (4, ['Agent UX Improvement', 'test-perf', 'triage/held'])]


class Definitions(unittest.TestCase):
    def test_the_24_labels_are_the_namespaces_and_values_of_the_design(self):
        design = (ROOT / 'docs' / 'design' / 'issue-triage.md').read_text(encoding='utf-8')
        expected = set()
        for namespace, values in re.findall(r'^\| `(\w+/)` \| (.+?) \|', design, re.M):
            expected |= {namespace + v for v in re.findall(r'`([^`]+)`', values)}
        names = [name for name, _, _ in labels.definitions()]
        self.assertEqual(len(names), 24)
        self.assertEqual(len(set(names)), 24)
        self.assertEqual(set(names), expected)
        counts = {ns: sum(n.startswith(ns) for n in names) for ns in ('class/', 'size/', 'pipeline/', 'triage/')}
        self.assertEqual(counts, {'class/': 10, 'size/': 4, 'pipeline/': 4, 'triage/': 6})
        for name, color, description in labels.definitions():
            self.assertRegex(color, '^[0-9A-F]{6}$', name)
            # GitHub refuses a label description over 100 characters.
            self.assertTrue(0 < len(description) <= 100, name)


class Commands(unittest.TestCase):
    def test_dry_run_prints_the_24_create_commands_and_runs_nothing(self):
        fake = FakeGh(ISSUES)
        printed = run(fake, '--repo', 'PakhomovAlexander/afactory')
        self.assertEqual(fake.calls, [])
        self.assertEqual([shlex.split(line) for line in printed],
                         labels.commands('PakhomovAlexander/afactory'))
        self.assertEqual(len(printed), 24)
        self.assertTrue(all(line.startswith('gh label create ') and ' --force ' in line for line in printed))

    def test_apply_runs_the_create_commands_and_deletes_nothing(self):
        fake = FakeGh(ISSUES)
        self.assertEqual(run(fake, '--apply', '--repo', 'o/r'), [])
        self.assertEqual(fake.calls, labels.commands('o/r'))
        self.assertTrue(all(c[:3] == ['gh', 'label', 'create'] for c in fake.calls))

    def test_without_repo_the_commands_name_no_repository(self):
        self.assertTrue(all('--repo' not in c for c in labels.commands()))


class Migrate(unittest.TestCase):
    EDITS = [['gh', 'issue', 'edit', '1', '--add-label', 'class/aix', '--remove-label', 'Agent UX Improvement',
              '--repo', 'o/r'],
             ['gh', 'issue', 'edit', '4', '--add-label', 'class/aix', '--remove-label', 'Agent UX Improvement',
              '--repo', 'o/r'],
             ['gh', 'issue', 'edit', '2', '--add-label', 'class/sdlc', '--remove-label', 'test-perf',
              '--repo', 'o/r'],
             ['gh', 'issue', 'edit', '4', '--add-label', 'class/sdlc', '--remove-label', 'test-perf',
              '--repo', 'o/r']]

    def test_migrate_without_apply_reads_open_issues_and_writes_nothing(self):
        fake = FakeGh(ISSUES)
        printed = run(fake, '--migrate', '--repo', 'o/r')
        self.assertEqual(fake.writes(), [])
        self.assertEqual(len(fake.reads()), 2)
        for call in fake.reads():
            self.assertIn('state=open', call)
            self.assertEqual(call[2], 'repos/o/r/issues')
        self.assertEqual([shlex.split(line) for line in printed], self.EDITS)

    def test_migrate_reads_every_page_so_no_issue_past_a_listing_limit_is_left_behind(self):
        numbers = list(range(1, 2 * labels.PAGE + 42))  # three pages, the last one short
        fake = FakeGh([(n, ['Agent UX Improvement']) for n in numbers] + [(9999, ['test-perf'])])
        run(fake, '--migrate', '--apply', '--repo', 'o/r')
        pages = [int(dict(a.split('=', 1) for a in c[c.index('-f') + 1::2])['page']) for c in fake.reads()]
        self.assertEqual(pages, [1, 2, 3, 1])
        edited = [int(c[3]) for c in fake.writes() if c[c.index('--remove-label') + 1] == 'Agent UX Improvement']
        self.assertEqual(edited, numbers)
        self.assertEqual([int(c[3]) for c in fake.writes() if c[c.index('--remove-label') + 1] == 'test-perf'], [9999])

    def test_migrate_skips_pull_requests_the_issues_endpoint_also_lists(self):
        class WithPr(FakeGh):
            def __call__(self, command, **kwargs):
                done = super().__call__(command, **kwargs)
                if command[:2] == ['gh', 'api'] and done.stdout:
                    rows = json.loads(done.stdout)
                    rows.append({'number': 77, 'labels': [{'name': 'Agent UX Improvement'}], 'pull_request': {}})
                    return subprocess.CompletedProcess(command, 0, json.dumps(rows), '')
                return done
        fake = WithPr([(1, ['Agent UX Improvement'])])
        run(fake, '--migrate', '--apply', '--repo', 'o/r')
        self.assertEqual([c[3] for c in fake.writes()], ['1'])

    def test_migrate_apply_maps_exactly_the_two_legacy_labels_and_removes_no_other(self):
        fake = FakeGh(ISSUES)
        run(fake, '--migrate', '--apply', '--repo', 'o/r')
        self.assertEqual(fake.writes(), self.EDITS)
        for call in fake.writes():
            removed = [call[i + 1] for i, arg in enumerate(call) if arg == '--remove-label']
            added = [call[i + 1] for i, arg in enumerate(call) if arg == '--add-label']
            self.assertEqual(len(removed), 1)
            self.assertEqual(added, [labels.MIGRATIONS[removed[0]]])
            self.assertNotIn('label', call[1:2], 'no gh label create/edit/delete in a migration')
        self.assertEqual(labels.MIGRATIONS, {'Agent UX Improvement': 'class/aix', 'test-perf': 'class/sdlc'})

    def test_migrate_with_no_legacy_issues_writes_nothing(self):
        fake = FakeGh([(3, ['enhancement'])])
        self.assertEqual(run(fake, '--migrate', '--apply'), [])
        self.assertEqual(fake.writes(), [])
        self.assertEqual(fake.calls[0][:3], ['gh', 'repo', 'view'], 'without --repo the current repository is read')

    def test_a_failed_issue_read_stops_before_any_write(self):
        def failing(command, **kwargs):
            calls.append(command)
            return subprocess.CompletedProcess(command, 1, '', 'HTTP 401')
        calls = []
        with self.assertRaises(SystemExit) as stop:
            run(failing, '--migrate', '--apply')
        self.assertIn('HTTP 401', str(stop.exception.code))
        self.assertEqual([c[:3] for c in calls], [['gh', 'repo', 'view']])


def fields(text):
    """Split an issue form's `body` into its items by the `  - type:` lines (no YAML parser)."""
    return [chunk for chunk in re.split(r'^  - (?=type: )', text.split('\nbody:\n', 1)[1], flags=re.M)
            if chunk.strip()]


def field(chunk, key):
    found = re.search(rf'^\s*{key}: (.+)$', chunk, re.M)
    return found and found.group(1).strip()


class Templates(unittest.TestCase):
    def read(self, name):
        text = (TEMPLATES / name).read_text(encoding='utf-8')
        self.assertNotIn('\t', text)
        for key in ('name', 'description', 'labels'):
            self.assertRegex(text, rf'(?m)^{key}: \S', (name, key))
        return text

    def test_the_raw_template_has_three_fields_one_label_and_no_title_prefix(self):
        text = self.read('raw.yml')
        self.assertRegex(text, r'(?m)^name: Raw note \(triaged automatically\)$')
        self.assertRegex(text, r'(?m)^labels: \[triage/pending\]$')
        self.assertNotRegex(text, r'(?m)^title:')
        items = fields(text)
        self.assertEqual([field(c, 'type') for c in items], ['markdown', 'textarea', 'textarea', 'textarea'])
        self.assertEqual([(field(c, 'label'), field(c, 'required')) for c in items[1:]],
                         [('What I saw', 'true'), ('Evidence', 'false'), ('Repro', 'false')])

    def test_the_bug_template_requires_only_what_happened_version_and_evidence(self):
        text = self.read('bug_report.yml')
        self.assertRegex(text, r'(?m)^labels: \[bug, triage/pending\]$')
        required = {field(c, 'id'): (field(c, 'label'), field(c, 'required'))
                    for c in fields(text) if field(c, 'type') != 'markdown'}
        self.assertEqual(required, {
            'version': ('af version', 'true'), 'platform': ('OS and target', 'false'),
            'policy': ('Relevant `.af/` policy', 'false'), 'command': ('Command', 'false'),
            'expected': ('Expected behaviour', 'false'), 'actual': ('What happened', 'true'),
            'evidence': ('Evidence', 'true')})

    def test_the_feature_template_also_enters_the_triage_queue(self):
        self.assertRegex(self.read('feature_request.yml'), r'(?m)^labels: \[enhancement, triage/pending\]$')


if __name__ == '__main__':
    unittest.main()
