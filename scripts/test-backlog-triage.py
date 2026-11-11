#!/usr/bin/env python3
"""Hermetic runner proofs: temporary Git objects, fake Tasks/API/clock/lease/notifier only.

No real gh, af, network, Provider, account, scheduler or deployment. The real report pipeline's
catalog/caps/renderer are covered independently by af::it task_triage.
"""
import copy
import datetime as dt
import importlib.util
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock
from urllib.parse import parse_qs, urlsplit

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


R = load('backlog_runner', ROOT / 'scripts/backlog-triage.py')
C = R.CHECK
HELPERS = load('check_fixtures', ROOT / 'scripts/test-backlog-check-triage.py')
NOW = dt.datetime(2026, 11, 2, 1, tzinfo=dt.timezone.utc)
REVERSED_FIXES = (
    'Reverted the change that fixed #436.',
    'The change that resolved #436 was reverted.',
    'Withdrawn: shipped fix for #436.',
    'The patch that implemented #436 has been withdrawn.',
    'Withdrew the change that resolved #436.',
    'Rolled back the release that fixed #436.',
    'Fixed #436.\nThe fix was subsequently rolled back.',
    'Backed out the patch that completed #436.',
    'Rescinded the change that fixed #436.',
    'Undid the change that resolved #436.',
)

HISTORICAL_FIXES = (
    'The change that fixed #432 was reversed.',
    'The change that fixed #432 needs investigation.',
    'The release which resolved #432 introduced another failure.',
    'An earlier patch implemented #432.',
    'We discussed how the team fixed #432.',
    'The report says #432 has been completed.',
    'The change that\nfixed #432 needs investigation.',
    'The report says\n#432 has been completed.',
    'Reversal of the fix: Fixed #432.',
    '- Fixed #432. The change was reversed.',
    'Fixed #432.\nWithdrawal of the fix followed.',
)

NONASSERTED_FIXES = (
    'Fixed #432?',
    'Fixed #432 was an incorrect claim.',
    'The report says: #432 has been completed.',
    'The report says:\n#432 has been completed.',
    'Fixed #432 according to the report.',
    'Status: Fixed #432?',
    'Status: #432 has been completed was an incorrect claim.',
)

# Each paragraph is captured from Git, then exercised through Runner.sweep twice.
CONTEXT_REFUSALS = (
    'Fixed #432: this was an incorrect claim.',
    'Fixed #432: e.g. completed?',
    'The report says:\n- Status: #432 has been completed.',
    'Fixed #432: i.e. completed? Other changes.',
    'Fixed #432. Was that accurate?',
    'Fixed #432. This was an inaccurate assertion.',
    'Fixed #432: a false statement.',
    'Fixed #432. The claim was withdrawn.',
    'Fixed #432. That statement was mistaken.',
    'The report says. Status: #432 has been completed.',
    'The report says; Status: #432 has been completed.',
    'According to the report:\n* Status:\n  #432 has been completed.',
    'Previously reported:\n1) Status: #432 has been completed.',
    'Quoted status:\n- Status: #432 has been completed.',
    'The summary states:\n- Status: #432 has been completed.',
    'Earlier release notes said; Status: #432 has been completed.',
    '> Status:\n- #432 has been completed.',
    '"Status:\n- #432 has been completed."',
)

CONTEXT_POSITIVES = (
    'Fixed #432: correct grouping.',
    'Other changes. Fixed #432.',
    'Changes; #432 has been completed.',
    '- Other changes\n- Fixed #432.',
)

DISTANT_CONTEXT = (
    'The report says:\n' + '- context\n' * 11 + '- Status: #432 has been completed.',
    'Is this fixed?\n' + '- context\n' * 11 + '- Fixed #432.',
    '- Fixed #432.\n' + '- context\n' * 11 + 'Is this fixed?',
    '- Status: #432 has been completed.\n' + '- context\n' * 11 + 'The report says.',
)

# Literal contract edges: the tests must not shrink when the production bound changes.
PARAGRAPH_CASES = {
    'line-edge': ('- context\n' * 127 + '- Fixed #432.', True),
    'line-before-over': ('- context\n' * 128 + '- Fixed #432.', False),
    'line-after-over': ('- Fixed #432.\n' + '- context\n' * 128, False),
    'byte-edge': ('- Fixed #432.\n- ' + 'é' * 8184, True),
    'byte-before-over': ('- ' + 'é' * 8185 + '\n- Fixed #432.', False),
    'byte-after-over': ('- Fixed #432.\n- ' + 'é' * 8185, False),
    'citation-byte-over': ('Fixed #432: ' + 'é' * 8192, False),
    'blank-boundaries': ('Is this fixed?\n\n- Fixed #432.\n \t\nThe report says.', True),
}

STATUS_FIXES = (
    'Status: #432 has been completed.',
    '- Status: #432 has been completed.',
    '- Status:\n  #432 has been completed.',
)


def kernel_bytes(value):
    out = []
    R.kernel_contracts._kernel_canonical(value, out, 'test CAS')
    return b''.join(out)


def put_kernel(task, value):
    if isinstance(value, dict) and 'artifact_id' in value:
        value['content_id'] = R.kernel_contracts.kernel_digest(R.kernel_contracts.KERNEL_CONTENT_DOMAIN,
                                                              value['payload'], 'test payload')
        value['artifact_id'] = R.kernel_contracts.kernel_digest(R.kernel_contracts.KERNEL_ARTIFACT_DOMAIN,
                                {k: v for k, v in value.items() if k != 'artifact_id'}, 'test envelope')
        identity = value['artifact_id']
    else:
        identity = R.kernel_contracts.kernel_blob_id(kernel_bytes(value))
    data = kernel_bytes(value)
    hex_id = identity[7:]
    path = task.state / 'cas/objects' / hex_id[:2] / hex_id[2:]
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return identity


class Lease:
    def __init__(self):
        self.uncertain, self.valid, self.checked, self.cleaned = False, True, 0, False
        self.expected_identity = [1, 2]

    def check(self):
        self.checked += 1
        R.require(self.valid and not self.uncertain, 'lease uncertain')

    def guard(self):
        self.check()

    def confirm_clean(self):
        self.check()
        self.cleaned = True

    def release(self):
        pass


def issue(number=432, **fields):
    row = dict(number=number, title='Waiting tasks grouped as approval', body='See src/code.rs:3',
               labels=['bug', 'triage/pending', 'outside'], comments=[], events=[],
               state='open', updated_at='2026-11-02T01:00:00Z')
    row.update(fields)
    return row


class API:
    def __init__(self, rows=None):
        self.rows = {i['number']: copy.deepcopy(i) for i in (rows or [issue()])}
        self.writes, self.next_id, self.pull_rows = [], 100, []
        self.fail_labels = self.fail_comment = self.fail_before_comment = False
        self.principal = 'runner'
        self.before_write = None

    def principal_check(self):
        R.require(self.principal == 'runner', 'principal mismatch')

    def issue(self, number):
        return copy.deepcopy(self.rows[number])

    def open_issues(self, since=''):
        return [copy.deepcopy(i) for i in self.rows.values() if i['state'] == 'open' and i['updated_at'] >= since]

    def pulls(self, _manifest):
        return copy.deepcopy(self.pull_rows)

    def set_labels(self, number, desired, current):
        if self.before_write:
            self.before_write()
        self.writes.append(('labels', number, desired))
        self.rows[number]['labels'] = list(desired)
        events = self.rows[number]['events']
        for name in sorted(set(current) ^ set(desired)):
            events.append(dict(id=max([e['id'] for e in events] + [0]) + 1, actor='runner',
                               event='labeled' if name in desired else 'unlabeled', label=name,
                               created_at=NOW.isoformat()))
        if self.fail_labels:
            self.fail_labels = False
            raise R.Ambiguous('lost label response')

    def comment(self, number, body, comment_id=None):
        if self.before_write:
            self.before_write()
        self.writes.append(('comment', number, body, comment_id))
        if self.fail_before_comment:
            self.fail_before_comment = False
            raise R.Ambiguous('request outcome unknown')
        if comment_id is None:
            comment_id = self.next_id
            self.next_id += 1
            self.rows[number]['comments'].append(dict(id=comment_id, author='runner', body=body,
                                                       created_at=NOW.isoformat()))
        else:
            matches = [c for c in self.rows[number]['comments'] if c['id'] == comment_id]
            R.require(len(matches) == 1, 'missing comment')
            matches[0]['body'] = body
        if self.fail_comment:
            self.fail_comment = False
            raise R.Ambiguous('lost comment response')
        return comment_id


class Tasks:
    def __init__(self):
        self.calls = self.collects = 0
        self.change = None
        self.failure, self.unknown, self.security = False, False, None
        self.needs_info = []

    def execute(self, gen, journal, key, now):
        self.calls += 1
        assert journal.data['generations'][key]['status'] == 'claimed'
        assert R.strict_json(journal.path.read_bytes())['generations'][key]['status'] == 'claimed'
        journal.reserve(key, now)
        assert R.strict_json(journal.path.read_bytes())['ledger'][key]['reserved'] == R.TOKENS
        gen['status'] = 'admitting'
        journal.save()
        if self.change:
            self.change()
        return self.collect(gen, journal, key)

    def collect(self, gen, journal, key):
        self.collects += 1
        journal.settle(key, dict(chargeable_tokens='1000', phase=dict(kind='finished'),
                                 unknown_usage_attempts=1 if self.unknown else 0))
        if self.unknown:
            raise R.Refused('Task usage unknown')
        if self.failure:
            raise R.TaskFailure('Task did not supply checked evidence')
        author = HELPERS.doc(issue=gen['identity']['issue'], body_sha256=R.text_hash(gen['input']['body']))
        verifier = copy.deepcopy(author)
        verifier['needs_info'] = self.needs_info
        if self.security == 'author':
            author['class'] = 'security'
        if self.security == 'verifier':
            verifier['class'] = 'security'
        return dict(author=author, evaluation=HELPERS.evaluation(block=verifier),
                    document_id=HELPERS.DOCUMENT_ID, evaluation_id='sha256:' + '6' * 64)


def fixture_git_environment():
    # Whitelist rather than copy: GIT_CONFIG_PARAMETERS, GIT_CONFIG_COUNT/KEY/VALUE,
    # GIT_DIR, templates, credentials and other host injection cannot enter setup.
    return dict(R.git_environment(), GIT_CONFIG_SYSTEM=os.devnull,
                GIT_AUTHOR_NAME='Fixture', GIT_AUTHOR_EMAIL='fixture@example.invalid',
                GIT_COMMITTER_NAME='Fixture', GIT_COMMITTER_EMAIL='fixture@example.invalid',
                GIT_AUTHOR_DATE='2026-11-02T01:00:00+0000', GIT_COMMITTER_DATE='2026-11-02T01:00:00+0000',
                TZ='UTC')


def fixture_git_argv(repo, *args):
    return ['git', '-c', 'core.hooksPath=/dev/null', '-c', 'commit.gpgsign=false',
            '-c', 'tag.gpgsign=false', '-c', 'core.autocrlf=false',
            '-c', 'init.defaultBranch=main', '-C', str(repo), *args]


def fixture_git(repo, *args):
    return subprocess.run(fixture_git_argv(repo, *args), check=True, capture_output=True,
                          env=fixture_git_environment()).stdout


class MemoryManifest:
    """Coordinator inputs only; Sources exercises real capture, text and reachability."""
    def __init__(self, repo, processes):
        self.repo, self.processes, self.main = str(repo), processes, HELPERS.MAIN
        self.files = {'src/code.rs': '\n'.join('line %d' % i for i in range(1, 401)) + '\n',
                      'docs/design/issue-triage.md': (ROOT / 'docs/design/issue-triage.md').read_text()}
        self.entries = {path: ('100644', 'blob', 'fake') for path in self.files}

    def text(self, path):
        R.require(path in self.files, 'path absent from fake manifest')
        return self.files[path]

    def citations(self, number):
        historical = R.repo_lines('\n\n'.join(HISTORICAL_FIXES))
        return copy.deepcopy({
            432: [dict(path='CHANGELOG.md', line=3, start=1,
                       text='# Release\n\n- Fixed #432: correct task grouping.')] + [
                dict(path='changelog.d/historical.md', line=i + 1, start=max(0, i - 10) + 1,
                     text='\n'.join(historical[max(0, i - 10):i + 11]))
                for i, line in enumerate(historical) if '432' in R.CITE.findall(line)],
            433: [dict(path='docs/adr/0001.md', line=3, start=1, text='# ADR\n\n#433 is not done here.')],
        }.get(number, []))

    def sweep_citations(self, number):
        # Coordinator-only fake: complete paragraphs; real expansion is exercised by GitFixture.
        return self.citations(number)


class Fixture(unittest.TestCase):
    real_git = False

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve() / 'state'
        self.root.mkdir(mode=0o700)
        self.lease = Lease()
        self.processes = R.Processes(self.lease, time.monotonic() + 5400)
        self.addCleanup(self.processes.clean)
        self.repo = self.root / 'unused-repo'
        self.manifest = MemoryManifest(self.repo, self.processes)
        self.main = self.manifest.main
        if not self.real_git:
            def reach(manifest, pulls):
                adapter = HELPERS.Reach(pulls=pulls)
                adapter.main = manifest.main
                return adapter
            self.reach = mock.patch.object(R, 'Reach', side_effect=reach)
            self.reach.start()
            self.addCleanup(self.reach.stop)
        self.journal = R.Journal(self.root, 'owner/repo', 'runner', 'Europe/Moscow', clock=lambda: self.clock)
        self.addCleanup(self.journal.close)
        self.api, self.tasks, self.notifications, self.output = API(), Tasks(), [], []
        self.clock = NOW
        self.runner = R.Runner(self.journal, self.api, self.tasks, lambda: self.manifest, self.lease,
                               lambda n: self.notifications.append(n) or True, lambda: self.clock,
                               'owner', apply=True, emit=self.output.append)
        # Fail immediately if any production transport is accidentally reached.
        self.network = mock.patch.object(socket.socket, 'connect', side_effect=AssertionError('real network forbidden'))
        self.network.start()
        self.addCleanup(self.network.stop)
        self.real_popen = subprocess.Popen
        def only_owned_fake(argv, *args, **kwargs):
            allowed = (self.real_git and argv[0] == 'git') or argv[0] == str(self.root / 'fake-af')
            self.assertTrue(allowed, 'real gh/af/Provider invocation forbidden: %s' % argv[0])
            return self.real_popen(argv, *args, **kwargs)
        self.popen = mock.patch.object(subprocess, 'Popen', side_effect=only_owned_fake)
        self.popen.start()
        self.addCleanup(self.popen.stop)

    def queue(self, event='opened', number=432):
        self.journal.data['events'][event] = dict(issue=number, status='queued')
        self.journal.save()
        self.runner.process(event)
        return self.journal.data['events'][event].get('generation')

    def gen(self):
        key = next(iter(self.journal.data['generations']))
        return key, self.journal.data['generations'][key]

    def bundle(self, row=None):
        return R.assemble(self.manifest, row or issue(), [], [dict(number=433, title='Task waits')])


class GitFixture(Fixture):
    real_git = True

    @classmethod
    def setUpClass(cls):
        cls.shared = tempfile.TemporaryDirectory()
        cls.repo = Path(cls.shared.name).resolve() / 'repo'
        cls.repo.mkdir()
        (cls.repo / 'src').mkdir()
        (cls.repo / 'docs/design').mkdir(parents=True)
        (cls.repo / 'docs/adr').mkdir()
        (cls.repo / 'changelog.d').mkdir()
        (cls.repo / 'src/code.rs').write_text('\n'.join('line %d' % i for i in range(1, 401)) + '\n')
        (cls.repo / 'docs/design/issue-triage.md').write_bytes((ROOT / 'docs/design/issue-triage.md').read_bytes())
        (cls.repo / 'src/unicode_lines').write_text('a\x85b\u2028c')
        (cls.repo / 'src/crlf').write_bytes(b'a\r\nb\r')
        (cls.repo / 'src/binary').write_bytes(b'\0binary\n')
        (cls.repo / 'src/nontext').write_bytes(b'\xff\n')
        (cls.repo / 'b.md').write_text('Must not read a suffix of an unsafe reference.\n')
        (cls.repo / 'src/link').symlink_to('code.rs')
        (cls.repo / 'src/50%-off').write_text('percent\n')
        (cls.repo / 'src/кириллица').write_text('unicode\n')
        (cls.repo / 'CHANGELOG.md').write_text('# Release\n\n- Fixed #432: correct task grouping.\n')
        (cls.repo / 'changelog.d/reversed.md').write_text('\n\n'.join(REVERSED_FIXES) + '\n')
        (cls.repo / 'changelog.d/historical.md').write_text('\n\n'.join(HISTORICAL_FIXES) + '\n')
        if getattr(cls, 'completion_clauses', False):
            (cls.repo / 'changelog.d/nonasserted.md').write_text('\n\n'.join(NONASSERTED_FIXES) + '\n')
            (cls.repo / 'changelog.d/context-refusals.md').write_text('\n\n'.join(CONTEXT_REFUSALS) + '\n')
            (cls.repo / 'changelog.d/context-positives.md').write_text('\n\n'.join(CONTEXT_POSITIVES) + '\n')
            (cls.repo / 'changelog.d/status.md').write_text('\n\n'.join(STATUS_FIXES) + '\n')
            for index, text in enumerate(DISTANT_CONTEXT):
                (cls.repo / ('changelog.d/distant-%d.md' % index)).write_text(text)
            for name, (text, _positive) in PARAGRAPH_CASES.items():
                (cls.repo / ('changelog.d/paragraph-%s.md' % name)).write_text(text)
        citation_lines = ['context %d' % n for n in range(35)]
        citation_lines[17] = 'Fixed #434: the entire issue.'
        (cls.repo / 'changelog.d/citation.md').write_text('\n'.join(citation_lines))
        (cls.repo / 'docs/adr/0001.md').write_text('# ADR\n\n#433 is not done here.\n')
        for directory in ('docs', 'changelog.d', 'docs/adr'):
            for name in ('a:b.md', 'a\\b.md', 'a b.md', 'a:12 b.md'):
                (cls.repo / directory / name).write_text('Captured %s/%s\nFixed #435.\n' % (directory, name))
        for args in (('init', '-q', '--template='), ('add', '.'), ('commit', '-qm', 'fixture')):
            fixture_git(cls.repo, *args)
        cls.main = fixture_git(cls.repo, 'rev-parse', 'HEAD').decode().strip()
        fixture_git(cls.repo, 'update-index', '--add', '--cacheinfo', '160000,' + cls.main + ',src/gitlink')
        fixture_git(cls.repo, 'commit', '-qm', 'gitlink fixture')
        cls.main = fixture_git(cls.repo, 'rev-parse', 'HEAD').decode().strip()
        (cls.repo / 'src/untracked').write_text('Not in the captured Manifest.\n')

    @classmethod
    def tearDownClass(cls):
        cls.shared.cleanup()

    def setUp(self):
        super().setUp()
        self.repo = type(self).repo
        self.main = type(self).main
        self.manifest = R.Manifest(self.repo, self.main, self.processes)


class CompleteClauses(GitFixture):
    completion_clauses = True

    def assert_captured_paragraph_replay(self, path, positive, expected_text=None, missing=None):
        # Restrict the shared fixture's candidates without replacing Git capture or expansion.
        # Overwrite the live file: only the pinned blob may supply either boundary.
        live = self.repo / path
        original = live.read_bytes()
        live.write_text('Fixed #432.\n')
        self.addCleanup(live.write_bytes, original)
        candidates = [c for c in self.manifest.citations(432) if c['path'] == path]
        self.assertEqual(len(candidates), 1)
        for replay in (False, True):
            if replay:
                self.journal.close()
                self.journal = R.Journal(self.root, 'owner/repo', 'runner', 'Europe/Moscow',
                                         clock=lambda: self.clock)
                self.addCleanup(self.journal.close)
                self.runner.journal = self.journal
                self.manifest = R.Manifest(self.repo, self.main, self.processes)
            with mock.patch.object(self.manifest, 'citations', return_value=candidates):
                cites = self.manifest.sweep_citations(432)
                if missing:
                    self.assertIn(missing, cites[0]['missing_evidence'])
                    self.assertNotIn('text', cites[0])
                else:
                    self.assertNotIn('missing_evidence', cites[0])
                    if expected_text:
                        self.assertEqual(cites[0]['text'], expected_text)
                self.runner.manifest_factory = lambda: self.manifest
                self.runner.sweep()
            if not replay:
                retained = copy.deepcopy(self.journal.data['owner_review'])
                writes = copy.deepcopy(self.api.writes)
            self.assertEqual(self.journal.data['owner_review'], retained)
            self.assertEqual(self.api.writes, writes)
            self.assertEqual([w[0] for w in writes], ['labels', 'comment'] if positive else [])
            if positive:
                self.assertEqual(retained, {})
                self.assertIn('triage/maybe-fixed', self.api.rows[432]['labels'])
                self.assertEqual(len(self.api.rows[432]['comments']), 1)
            else:
                self.assertEqual(len(retained), 1)
                if expected_text and not missing:
                    self.assertIn(' '.join(expected_text.splitlines()), next(iter(retained.values()))['citation'])
                self.assertEqual(self.api.rows[432]['labels'], issue(432)['labels'])
                self.assertEqual(self.api.rows[432]['comments'], [])
            self.assertEqual(self.api.rows[432]['state'], 'open')
            self.assertEqual(self.tasks.calls, 0)

    def test_captured_distant_attribution_before_completion_is_private_on_replay(self):
        self.assert_captured_paragraph_replay('changelog.d/distant-0.md', False, DISTANT_CONTEXT[0])

    def test_captured_distant_question_before_completion_is_private_on_replay(self):
        self.assert_captured_paragraph_replay('changelog.d/distant-1.md', False, DISTANT_CONTEXT[1])

    def test_captured_distant_question_after_completion_is_private_on_replay(self):
        self.assert_captured_paragraph_replay('changelog.d/distant-2.md', False, DISTANT_CONTEXT[2])

    def test_captured_distant_attribution_after_completion_is_private_on_replay(self):
        self.assert_captured_paragraph_replay('changelog.d/distant-3.md', False, DISTANT_CONTEXT[3])

    def test_captured_paragraph_line_and_utf8_byte_bounds_on_replay(self):
        self.assertEqual((R.SWEEP_PARAGRAPH_LINES, R.SWEEP_PARAGRAPH_BYTES), (128, 16384))
        for name, (text, positive) in PARAGRAPH_CASES.items():
            with self.subTest(name=name):
                # Each subcase starts from independent public and durable issue state.
                self.api.rows[432] = issue(432)
                self.api.writes.clear()
                self.journal.data['owner_review'].clear()
                self.journal.data['issues'].clear()
                self.journal.save()
                self.assert_captured_paragraph_replay('changelog.d/paragraph-%s.md' % name, positive,
                    text if name != 'blank-boundaries' else None,
                    None if positive else 'sweep paragraph ' + ('line' if name.startswith('line') else 'byte') + ' bound')

    def test_captured_paragraph_read_refusal_retains_private_evidence_on_replay(self):
        candidates = [c for c in self.manifest.citations(432) if c['path'] == 'changelog.d/distant-0.md']
        with mock.patch.object(R.Manifest, 'text', side_effect=R.Refused('captured blob unavailable')):
            # The citation is known, but its complete captured context cannot be read.
            with mock.patch.object(R.Manifest, 'citations', return_value=candidates):
                self.assert_captured_paragraph_replay('changelog.d/distant-0.md', False,
                                                      missing='captured blob unavailable')

    def test_captured_questions_disavowals_and_reported_status_have_zero_sweep_effects(self):
        citations = [c for c in self.manifest.citations(432)
                     if c['path'] == 'changelog.d/nonasserted.md']
        self.assertEqual(len(citations), len(NONASSERTED_FIXES))
        positive, review = R.sweep_evidence(citations, [], 432)
        self.assertEqual(positive, [])
        self.assertEqual(len(review), len(NONASSERTED_FIXES))
        with mock.patch.object(self.manifest, 'citations', side_effect=lambda n: citations if n == 432 else []):
            self.runner.sweep()
            self.runner.sweep()
        retained = list(self.journal.data['owner_review'].values())
        self.assertEqual(len(retained), len(NONASSERTED_FIXES))
        for text in NONASSERTED_FIXES:
            self.assertTrue(any(' '.join(text.splitlines()) in entry['citation'] for entry in retained))
        self.assertEqual(self.api.writes, [])
        self.assertEqual(self.api.rows[432]['comments'], [])
        self.assertEqual(self.api.rows[432]['labels'], issue(432)['labels'])
        self.assertEqual(self.api.rows[432]['state'], 'open')
        self.assertEqual(self.tasks.calls, 0)

    def test_captured_paragraph_context_vetoes_claims_before_splitting_on_replay(self):
        citations = [c for c in self.manifest.citations(432)
                     if c['path'] == 'changelog.d/context-refusals.md']
        self.assertEqual(len(citations), len(CONTEXT_REFUSALS))
        for cite, text in zip(citations, CONTEXT_REFUSALS):
            with self.subTest(text=text):
                positive, review = R.sweep_evidence([cite], [], 432)
                self.assertEqual(positive, [])
                self.assertEqual(len(review), 1)
                with mock.patch.object(self.manifest, 'citations', return_value=[cite]):
                    self.runner.sweep()
                    retained = copy.deepcopy(self.journal.data['owner_review'])
                    # Round-trip durable private evidence before replay.
                    self.journal.close()
                    self.journal = R.Journal(self.root, 'owner/repo', 'runner', 'Europe/Moscow',
                                             clock=lambda: self.clock)
                    self.addCleanup(self.journal.close)
                    self.runner.journal = self.journal
                    self.runner.sweep()
                self.assertEqual(self.journal.data['owner_review'], retained)
                self.assertTrue(any(' '.join(text.splitlines()) in entry['citation']
                                    for entry in retained.values()))
                self.assertEqual(self.api.writes, [])
                self.assertEqual(self.api.rows[432]['comments'], [])
                self.assertEqual(self.api.rows[432]['labels'], issue(432)['labels'])
                self.assertEqual(self.api.rows[432]['state'], 'open')
                self.assertEqual(self.tasks.calls, 0)

    def assert_captured_independent_completion_published_once(self, index):
        citations = [c for c in self.manifest.citations(432)
                     if c['path'] == 'changelog.d/context-positives.md']
        self.assertEqual(len(citations), len(CONTEXT_POSITIVES))
        cite = citations[index]
        with mock.patch.object(self.manifest, 'citations', return_value=[cite]):
            self.runner.sweep()
            writes = copy.deepcopy(self.api.writes)
            self.runner.sweep()
        self.assertEqual([w[0] for w in writes], ['labels', 'comment'])
        self.assertEqual(self.api.writes, writes)
        self.assertEqual(len(self.api.rows[432]['comments']), 1)
        self.assertIn('triage/maybe-fixed', self.api.rows[432]['labels'])
        self.assertIn(' '.join(CONTEXT_POSITIVES[index].splitlines()),
                      self.api.rows[432]['comments'][0]['body'])
        self.assertEqual(self.journal.data['owner_review'], {})
        self.assertEqual(self.api.rows[432]['state'], 'open')
        self.assertEqual(self.tasks.calls, 0)

    def test_captured_affirmative_colon_publishes_once(self):
        self.assert_captured_independent_completion_published_once(0)

    def test_captured_independent_sentence_publishes_once(self):
        self.assert_captured_independent_completion_published_once(1)

    def test_captured_independent_semicolon_publishes_once(self):
        self.assert_captured_independent_completion_published_once(2)

    def test_captured_independent_bullet_publishes_once(self):
        self.assert_captured_independent_completion_published_once(3)

    def assert_captured_status_published_once(self, index):
        citations = [c for c in self.manifest.citations(432) if c['path'] == 'changelog.d/status.md']
        self.assertEqual(len(citations), len(STATUS_FIXES))
        cite = citations[index]
        with mock.patch.object(self.manifest, 'citations', return_value=[cite]):
            self.runner.sweep()
            writes = copy.deepcopy(self.api.writes)
            self.runner.sweep()
        self.assertEqual(self.api.writes, writes)
        self.assertEqual([w[0] for w in writes], ['labels', 'comment'])
        self.assertEqual(self.api.rows[432]['labels'],
                         sorted((set(issue(432)['labels']) - {'triage/pending'}) | {'triage/maybe-fixed'}))
        comments = self.api.rows[432]['comments']
        self.assertEqual(len(comments), 1)
        self.assertIn('changelog.d/status.md:%d' % cite['line'], comments[0]['body'])
        self.assertIn(' '.join(STATUS_FIXES[index].splitlines()), comments[0]['body'])
        self.assertIn('triage-sweep evidence=', comments[0]['body'])
        self.assertEqual(self.journal.data['owner_review'], {})
        self.assertEqual(self.api.rows[432]['state'], 'open')
        self.assertEqual(self.tasks.calls, 0)

    def test_captured_bare_status_completion_publishes_once(self):
        self.assert_captured_status_published_once(0)

    def test_captured_bulleted_status_completion_publishes_once(self):
        self.assert_captured_status_published_once(1)

    def test_captured_wrapped_status_completion_publishes_once(self):
        self.assert_captured_status_published_once(2)


class Sources(GitFixture):
    def test_captured_reverted_or_withdrawn_fixes_are_owner_review_without_public_mutation(self):
        self.api.rows = {436: issue(436)}
        citations = self.manifest.citations(436)
        self.assertEqual(len(citations), len(REVERSED_FIXES))
        positive, review = R.sweep_evidence(citations, [], 436)
        self.assertEqual(positive, [])
        self.assertEqual(len(review), len(REVERSED_FIXES))
        self.runner.sweep()
        self.runner.sweep()
        retained = self.journal.data['owner_review'].values()
        self.assertEqual(len(retained), len(REVERSED_FIXES))
        for text in REVERSED_FIXES:
            self.assertTrue(any(' '.join(text.splitlines()) in entry['citation'] for entry in retained))
        self.assertEqual(self.api.writes, [])
        self.assertEqual(self.api.rows[436]['comments'], [])
        self.assertEqual(self.api.rows[436]['labels'], issue(436)['labels'])
        self.assertEqual(self.api.rows[436]['state'], 'open')
        self.assertEqual(self.tasks.calls, 0)

    def test_captured_historical_completion_assertions_have_zero_sweep_effects(self):
        # Read real captured blobs; the separate positive CHANGELOG entry remains
        # covered by the positive sweep controls, outside this source selection.
        citations = [c for c in self.manifest.citations(432)
                     if c['path'] == 'changelog.d/historical.md']
        self.assertEqual(len(citations), len(HISTORICAL_FIXES))
        positive, review = R.sweep_evidence(citations, [], 432)
        self.assertEqual(positive, [])
        self.assertEqual(len(review), len(HISTORICAL_FIXES))
        with mock.patch.object(self.manifest, 'citations', side_effect=lambda n: citations if n == 432 else []):
            self.runner.sweep()
            self.runner.sweep()
        retained = list(self.journal.data['owner_review'].values())
        self.assertEqual(len(retained), len(HISTORICAL_FIXES))
        for text in HISTORICAL_FIXES:
            self.assertTrue(any(' '.join(text.splitlines()) in entry['citation'] for entry in retained))
        self.assertEqual(self.api.writes, [])
        self.assertEqual(self.api.rows[432]['comments'], [])
        self.assertEqual(self.api.rows[432]['labels'], issue(432)['labels'])
        self.assertEqual(self.api.rows[432]['state'], 'open')
        self.assertEqual(self.tasks.calls, 0)

    def test_fixture_git_environment_and_commit_identity_are_isolated_and_deterministic(self):
        poison = dict(GIT_CONFIG_GLOBAL='/absent/host-config', GIT_CONFIG_SYSTEM='/absent/system-config',
                      GIT_CONFIG_PARAMETERS="'commit.gpgsign=true'", GIT_CONFIG_COUNT='1',
                      GIT_CONFIG_KEY_0='core.hooksPath', GIT_CONFIG_VALUE_0='/host-hooks',
                      GIT_CONFIG='/host-config', GIT_DIR='/host-repo', GIT_TEMPLATE_DIR='/host-templates',
                      GIT_AUTHOR_NAME='Host', GIT_COMMITTER_DATE='yesterday')
        with mock.patch.dict(os.environ, poison):
            env = fixture_git_environment()
            self.assertEqual(env['GIT_CONFIG_GLOBAL'], os.devnull)
            self.assertEqual(env['GIT_CONFIG_SYSTEM'], os.devnull)
            self.assertEqual(env['GIT_CONFIG_NOSYSTEM'], '1')
            injected = set(poison) - {'GIT_CONFIG_GLOBAL', 'GIT_CONFIG_SYSTEM',
                                     'GIT_AUTHOR_NAME', 'GIT_COMMITTER_DATE'}
            self.assertTrue(injected.isdisjoint(env))
            with mock.patch.object(subprocess, 'run', wraps=subprocess.run) as run:
                commit = fixture_git(self.repo, 'cat-file', 'commit', self.main).decode()
            self.assertEqual(run.call_args.kwargs['env'], env)
            argv = run.call_args.args[0]
            for setting in ('core.hooksPath=/dev/null', 'commit.gpgsign=false', 'tag.gpgsign=false'):
                self.assertIn(setting, [argv[i + 1] for i, arg in enumerate(argv) if arg == '-c'])
        identity = 'Fixture <fixture@example.invalid> %d +0000' % NOW.timestamp()
        self.assertIn('\nauthor ' + identity + '\n', commit)
        self.assertIn('\ncommitter ' + identity + '\n', commit)

    def test_in_memory_coordinator_sources_match_git_without_running_processes(self):
        memory = MemoryManifest(self.repo, self.processes)
        for number in (432, 433):
            captured = self.bundle(issue(number))
            with mock.patch.object(subprocess, 'Popen', side_effect=AssertionError('coordinator ran a process')):
                self.assertEqual(R.assemble(memory, issue(number), [], [dict(number=433, title='Task waits')]), captured)

    def test_real_reachability_adapter_checks_captured_main_and_refuses_absent_commits(self):
        reach = R.Reach(self.manifest, {7})
        self.assertEqual(reach.main, self.main)
        self.assertTrue(reach.commit(self.main))
        self.assertFalse(reach.commit('f' * 40))
        self.assertTrue(reach.pull(7))
        self.assertFalse(reach.pull(8))

    def test_citation_has_ten_surrounding_lines_from_captured_object(self):
        cite, = self.manifest.citations(434)
        self.assertEqual((cite['line'], cite['start']), (18, 8))
        self.assertEqual(len(cite['text'].splitlines()), 21)
        self.assertEqual(cite['text'].splitlines()[0], 'context 7')
        self.assertEqual(cite['text'].splitlines()[-1], 'context 27')

    def test_production_pull_reader_binds_closing_refs_to_this_repository_and_main(self):
        client = R.GitHub('owner/repo', 'fake', self.lease, time.monotonic() + 5400, 'runner')
        nodes = [dict(number=7, title='Fix #432', body='', state='MERGED', mergedAt=NOW.isoformat(),
                      mergeCommit=dict(oid=self.main), closingIssuesReferences=dict(pageInfo=dict(hasNextPage=False),
                          nodes=[dict(number=432, repository=dict(nameWithOwner='owner/repo')),
                                 dict(number=9, repository=dict(nameWithOwner='other/repo'))]))]
        client.call = mock.Mock(return_value=dict(data=dict(repository=dict(pullRequests=dict(
            pageInfo=dict(hasNextPage=False, endCursor=None), nodes=nodes)))))
        rows = client.pulls(self.manifest)
        self.assertEqual(rows[0]['closing'], [432])
        self.assertTrue(rows[0]['merged'])
        self.assertEqual(client.call.call_args.args[:2], ('POST', '/graphql'))

    def test_canonical_colon_backslash_and_internal_space_excerpts_and_citations(self):
        for directory in ('docs', 'changelog.d', 'docs/adr'):
            for name in ('a:b.md', 'a\\b.md', 'a b.md', 'a:12 b.md'):
                path = directory + '/' + name
                with self.subTest(path=path):
                    captured = 'Captured %s\nFixed #435.' % path
                    for reference in (path + ':1', 'See ' + path + ':1', '`' + path + ':1`',
                                      '[' + path + ':1]'):
                        result = json.loads(self.bundle(issue(435, body=reference))['sources']['excerpts']['text'])
                        self.assertEqual(result['missing_evidence'], [])
                        self.assertEqual(result['excerpts'], [dict(path=path, line=1, start=1, text=captured)])
                    for separator in (', ', '; ', ',', ';'):
                        for first in ('src/code.rs:3', '[src/code.rs:3]'):
                            reference = first + separator + '[' + path + ':1]'
                            result = json.loads(self.bundle(issue(435, body=reference))['sources']['excerpts']['text'])
                            self.assertEqual(result['missing_evidence'], [])
                            self.assertEqual([(e['path'], e['line']) for e in result['excerpts']],
                                             [('src/code.rs', 3), (path, 1)])
                            self.assertIn('line 3', result['excerpts'][0]['text'])
                            self.assertEqual(result['excerpts'][1]['text'], captured)
                        reference = 'src/code.rs:3' + separator + path + ':1'
                        result = json.loads(self.bundle(issue(435, body=reference))['sources']['excerpts']['text'])
                        self.assertEqual(result['missing_evidence'], [])
                        self.assertEqual([e['path'] for e in result['excerpts']], ['src/code.rs', path])
                        self.assertEqual(result['excerpts'][1]['text'], captured)
        citations = self.manifest.citations(435)
        self.assertEqual({cite['path'] for cite in citations}, {
            directory + '/' + name for directory in ('changelog.d', 'docs/adr')
            for name in ('a:b.md', 'a\\b.md', 'a b.md', 'a:12 b.md')})
        for cite in citations:
            self.assertNotIn('missing_evidence', cite)
            self.assertEqual(cite['text'], 'Captured %s\nFixed #435.' % cite['path'])

    def test_punctuated_canonical_reference_then_quoted_reference_yields_both_excerpts(self):
        body = 'See src/code.rs:3. Then `docs/a b.md:1`'
        result = json.loads(self.bundle(issue(body=body))['sources']['excerpts']['text'])
        self.assertEqual(result['missing_evidence'], [])
        self.assertEqual([(e['path'], e['line']) for e in result['excerpts']],
                         [('src/code.rs', 3), ('docs/a b.md', 1)])
        self.assertIn('line 3', result['excerpts'][0]['text'])
        self.assertEqual(result['excerpts'][1]['text'], 'Captured docs/a b.md\nFixed #435.')

    def test_completed_reference_does_not_discard_attached_url_or_absolute_prefix(self):
        for prefix in ('https://example.invalid/', '/etc/'):
            body = 'src/code.rs:3.' + prefix + ' `docs/a b.md:1`'
            with self.subTest(body=body):
                result = json.loads(self.bundle(issue(body=body))['sources']['excerpts']['text'])
                self.assertEqual([(e['path'], e['line']) for e in result['excerpts']], [('src/code.rs', 3)])
                self.assertIn('line 3', result['excerpts'][0]['text'])
                self.assertEqual(result['missing_evidence'], [dict(
                    reference=prefix + ' `docs/a b.md:1', reason='unsafe bracket prefix refused')])

    def test_attached_prefix_after_punctuation_stays_unsafe_across_quoted_canonical_paths(self):
        for punctuation in ('.', '...', ',', ';'):
            for prefix in ('/etc/', '../', 'https://example.invalid/'):
                for gap in ('', ' '):
                    for path in ('src/code.rs', 'docs/a:b.md', 'docs/a\\b.md',
                                 'docs/a b.md', 'docs/a:12 b.md'):
                        reference = prefix + gap + '`' + path + ':1`'
                        body = 'src/code.rs:3' + punctuation + reference
                        with self.subTest(body=body):
                            result = json.loads(self.bundle(issue(body=body))['sources']['excerpts']['text'])
                            self.assertEqual([(e['path'], e['line']) for e in result['excerpts']],
                                             [('src/code.rs', 3)])
                            self.assertIn('line 3', result['excerpts'][0]['text'])
                            self.assertEqual(len(result['missing_evidence']), 1)
                            self.assertEqual(result['missing_evidence'][0]['reason'], 'unsafe bracket prefix refused')
                            # A dot run before / can also end in ../; retain that
                            # traversal authority rather than consume it as prose.
                            retained = '..' if punctuation == '...' and prefix == '/etc/' else ''
                            self.assertEqual(result['missing_evidence'][0]['reference'], retained + reference[:-1])

    def test_unsafe_quoted_numeric_colon_filename_is_one_refusal_without_suffix_entry(self):
        entries = {path: value for path, value in self.manifest.entries.items() if path != 'b.md'}
        with mock.patch.object(self.manifest, 'entries', entries):
            for opening, closing in (('`', '`'), ('<', '>'), ('"', '"')):
                reference = '/etc/' + opening + 'docs/a:12 b.md:1'
                body = 'src/code.rs:3.' + reference + closing
                with self.subTest(body=body):
                    result = json.loads(self.bundle(issue(body=body))['sources']['excerpts']['text'])
                    self.assertEqual([(e['path'], e['line']) for e in result['excerpts']], [('src/code.rs', 3)])
                    self.assertIn('line 3', result['excerpts'][0]['text'])
                    self.assertEqual(result['missing_evidence'], [dict(
                        reference=reference, reason='unsafe bracket prefix refused')])

    def test_punctuation_directly_before_quoted_reference_preserves_both_excerpts(self):
        for punctuation in ('.', '...', ',', ';'):
            for gap in ('', ' '):
                for quoted in ('`docs/a b.md:1`', '<docs/a b.md:1>', '"docs/a b.md:1"'):
                    body = 'src/code.rs:3' + punctuation + gap + quoted
                    with self.subTest(body=body):
                        result = json.loads(self.bundle(issue(body=body))['sources']['excerpts']['text'])
                        self.assertEqual([(e['path'], e['line']) for e in result['excerpts']],
                                         [('src/code.rs', 3), ('docs/a b.md', 1)])
                        self.assertIn('line 3', result['excerpts'][0]['text'])
                        self.assertEqual(result['excerpts'][1]['text'], 'Captured docs/a b.md\nFixed #435.')
                        self.assertEqual(result['missing_evidence'], [])

    def test_parser_punctuation_completes_entire_canonical_paths_before_markup_boundaries(self):
        paths = ['src/code.rs', 'docs/a:b.md', 'docs/a\\b.md', 'docs/a b.md', 'docs/a:12 b.md']
        for path in paths:
            for suffix in (':1', ':1-2'):
                for punctuation in ('.', ',', ';', '!', '?', ')', ']', '...'):
                    for quoted in ('`docs/a b.md:1`', '<docs/a b.md:1>', '"docs/a b.md:1"'):
                        body = 'See ' + path + suffix + punctuation + ' Then ' + quoted
                        with self.subTest(body=body):
                            result = json.loads(self.bundle(issue(body=body))['sources']['excerpts']['text'])
                            self.assertEqual(result['missing_evidence'], [])
                            self.assertEqual([(e['path'], e['line']) for e in result['excerpts']],
                                             [(path, 1), ('docs/a b.md', 1)])
                            self.assertIn('line 1' if path == 'src/code.rs' else 'Captured ' + path,
                                          result['excerpts'][0]['text'])
                            self.assertEqual(result['excerpts'][1]['text'], 'Captured docs/a b.md\nFixed #435.')

    def test_punctuated_suffix_of_unsafe_path_keeps_prefix_authority(self):
        for prefix in ('/etc/', '../', 'https://example.invalid/'):
            for path in ('src/code.rs', 'docs/a:b.md', 'docs/a\\b.md', 'docs/a b.md', 'docs/a:12 b.md'):
                for punctuation in ('.', ',', ';', '!', '?', ')', ']'):
                    for delimiter in (' ', '[', '`', '<', '"', '\x00', '\n'):
                        body = prefix + path + ':1' + punctuation + delimiter + '`docs/a b.md:1`'
                        with self.subTest(body=body):
                            result = json.loads(self.bundle(issue(body=body))['sources']['excerpts']['text'])
                            self.assertEqual(result['excerpts'], [])
                            self.assertEqual(len(result['missing_evidence']), 2)
                            self.assertEqual(result['missing_evidence'][-1], dict(
                                reference=body[:-1], reason='unsafe bracket prefix refused'))

    def test_separated_references_include_every_excerpt_and_preserve_unsafe_prefix_refusals(self):
        for separator in (', ', '; '):
            paths = ['src/code.rs', 'docs/design/issue-triage.md',
                     'docs/a:b.md', 'docs/a\\b.md', 'docs/a b.md', 'docs/a:12 b.md']
            result = json.loads(self.bundle(issue(body=separator.join(p + ':1' for p in paths)))
                                ['sources']['excerpts']['text'])
            self.assertEqual(result['missing_evidence'], [])
            self.assertEqual([e['path'] for e in result['excerpts']], paths)
            for excerpt in result['excerpts'][2:]:
                self.assertEqual(excerpt['text'], 'Captured %s\nFixed #435.' % excerpt['path'])
            for gap in ('', ' ', '\t', '\n', '\r', '\v', '\f', '\x00', '\x01', '\x1f',
                        ' x ', '\tx\n', ' x [\r'):
                for prefix in ('/etc/', '../', './', 'docs/../', 'https://example.invalid/', 'bad\\'):
                    reference = prefix + ' [' + gap + 'docs/a b.md:1]'
                    with self.subTest(separator=separator, reference=reference):
                        result = json.loads(self.bundle(issue(body='src/code.rs:3' + separator + reference))
                                            ['sources']['excerpts']['text'])
                        self.assertEqual([e['path'] for e in result['excerpts']], ['src/code.rs'])
                        self.assertIn('line 3', result['excerpts'][0]['text'])
                        self.assertEqual(len(result['missing_evidence']), 1)
                        self.assertEqual(result['missing_evidence'][0]['reference'], reference[:-1])
            for control in map(chr, range(0x20)):
                for prefix in ('/etc/', '../', './', 'docs/../', 'https://example.invalid/', 'bad\\'):
                    for inside in ('', ' x '):
                        reference = prefix + ' ' + control + ' [' + inside + 'docs/a b.md:1]'
                        with self.subTest(separator=separator, control=ord(control), prefix=prefix, inside=inside):
                            result = json.loads(self.bundle(issue(body='src/code.rs:3' + separator + reference))
                                                ['sources']['excerpts']['text'])
                            self.assertEqual([e['path'] for e in result['excerpts']], ['src/code.rs'])
                            self.assertIn('line 3', result['excerpts'][0]['text'])
                            self.assertEqual(len(result['missing_evidence']), 1)
                            self.assertEqual(result['missing_evidence'][0]['reference'], reference[:-1])
                            self.assertEqual(result['missing_evidence'][0]['reason'], 'unsafe bracket prefix refused')
            for ending in ('\t', '\n', '\r', '\x00', '\x01', '\x1f', '`', '>', '"'):
                reference = '/etc/ [ x docs/a b.md:1' + ending + ']'
                with self.subTest(separator=separator, reference=reference):
                    result = json.loads(self.bundle(issue(body='src/code.rs:3' + separator + reference))
                                        ['sources']['excerpts']['text'])
                    self.assertEqual([e['path'] for e in result['excerpts']], ['src/code.rs'])
                    self.assertEqual(len(result['missing_evidence']), 1)
                    self.assertEqual(result['missing_evidence'][0]['reference'], '/etc/ [ x docs/a b.md:1')
            for path in paths[2:]:
                for gap in ('', ' ', '\t', '\n', '\r'):
                    for first in ('src/code.rs:3' + separator, '[src/code.rs:3]' + separator,
                                  '[src/code.rs:3]', '[src/code.rs:3],', '[src/code.rs:3];',
                                  '`src/code.rs:3` ', '<src/code.rs:3> ', '"src/code.rs:3" '):
                        reference = first + '[' + gap + path + ':1]'
                        with self.subTest(reference=reference):
                            result = json.loads(self.bundle(issue(body=reference))['sources']['excerpts']['text'])
                            self.assertEqual(result['missing_evidence'], [])
                            self.assertEqual([e['path'] for e in result['excerpts']], ['src/code.rs', path])
                            self.assertEqual(result['excerpts'][1]['text'], 'Captured %s\nFixed #435.' % path)
            for prefix in ('/', '../', './', 'docs/../', 'https://example.invalid/',
                           'https://example.invalid/ ', '../ ', 'bad\\ '):
                for path in paths[2:]:
                    for reference in (prefix + path + ':1', prefix + '[' + path + ':1]'):
                        with self.subTest(separator=separator, reference=reference):
                            body = 'src/code.rs:3' + separator + reference
                            result = json.loads(self.bundle(issue(body=body))['sources']['excerpts']['text'])
                            self.assertEqual([e['path'] for e in result['excerpts']], ['src/code.rs'])
                            self.assertEqual(len(result['missing_evidence']), 1)
            for reference in ('/etc/ [ docs/a b.md:1]', '/etc/[ docs/a b.md:1]',
                              '../ [ docs/a b.md:1]', 'https://example.invalid/ [ docs/a b.md:1]',
                              'https://example.invalid/[ docs/a b.md:1]',
                              'bad\\ [ docs/a b.md:1]', '/etc/ [ [  docs/a b.md:1]',
                              '/etc/ [\u2003docs/a b.md:1]'):
                with self.subTest(separator=separator, reference=reference):
                    body = 'src/code.rs:3' + separator + reference
                    result = json.loads(self.bundle(issue(body=body))['sources']['excerpts']['text'])
                    self.assertEqual([e['path'] for e in result['excerpts']], ['src/code.rs'])
                    self.assertIn('line 3', result['excerpts'][0]['text'])
                    self.assertEqual(len(result['missing_evidence']), 1)
                    self.assertEqual(result['missing_evidence'][0]['reference'], reference[:-1])
        for reference in ('/etc/passwd:1', '/docs/a:12 b.md:1', 'missing:1'):
            result = json.loads(self.bundle(issue(body='src/code.rs:3 ' + reference))
                                ['sources']['excerpts']['text'])
            self.assertEqual([e['path'] for e in result['excerpts']], ['src/code.rs'])
            self.assertEqual(len(result['missing_evidence']), 1)

    def test_unsafe_prefix_stays_across_every_control_and_text_until_a_real_reference_boundary(self):
        for separator in (', ', '; ', ',', ';'):
            for control in map(chr, range(0x20)):
                for prefix in ('/etc/', '../', 'https://example.invalid/'):
                    for after in ('', 'x', ' x ', ' ordinary text '):
                        for inside in ('', ' x '):
                            reference = prefix + control + after + '[' + inside + 'docs/a b.md:1]'
                            with self.subTest(separator=separator, control=ord(control), prefix=prefix,
                                              after=after, inside=inside):
                                result = json.loads(self.bundle(issue(body='src/code.rs:3' + separator + reference))
                                                    ['sources']['excerpts']['text'])
                                self.assertEqual([e['path'] for e in result['excerpts']], ['src/code.rs'])
                                self.assertIn('line 3', result['excerpts'][0]['text'])
                                self.assertEqual(result['missing_evidence'], [dict(
                                    reference=reference[:-1], reason='unsafe bracket prefix refused')])

    def test_only_completed_canonical_reference_boundaries_release_prefix_authority(self):
        for separator in (',', ';', ', ', '; '):
            for first in ('src/code.rs:3', '[src/code.rs:3]', '[docs/a b.md:1]'):
                with self.subTest(first=first, separator=separator):
                    body = first + separator + '[docs/a b.md:1]'
                    result = json.loads(self.bundle(issue(body=body))['sources']['excerpts']['text'])
                    self.assertEqual([e['path'] for e in result['excerpts']],
                                     ['docs/a b.md' if first.startswith('[docs/') else 'src/code.rs', 'docs/a b.md'])
                    self.assertEqual(result['missing_evidence'], [])
            hostile = ('x', ' x ', ' [x] ', ' [] ', ' [ [ x ] ] ', ', x;',
                       '`x`', '<x>', '"x"', 'src/code.rs:3,', '[src/code.rs:3];',
                       'missing:1,', ' [missing:1];')
            for intervening in hostile + tuple(chr(value) + 'x' for value in range(0x20, 0x80)):
                with self.subTest(separator=separator, intervening=intervening):
                    reference = '/etc/' + intervening + '[docs/a b.md:1]'
                    result = json.loads(self.bundle(issue(body='src/code.rs:3' + separator + reference))
                                        ['sources']['excerpts']['text'])
                    self.assertEqual([e['path'] for e in result['excerpts']], ['src/code.rs'])
                    expected = 2 if ':1' in intervening or ':3' in intervening else 1
                    self.assertEqual(len(result['missing_evidence']), expected)
                    reasons = ['unsafe bracket prefix refused'] * expected
                    if intervening in ('src/code.rs:3,', 'missing:1,'):
                        reasons[0] = 'path absent from captured manifest'
                    self.assertEqual([e['reason'] for e in result['missing_evidence']], reasons)
                    self.assertEqual(result['missing_evidence'][-1]['reference'], reference[:-1])

    def test_safe_excerpt_is_from_pinned_manifest_and_200_existing_lines(self):
        bundle = self.bundle()
        excerpts = json.loads(bundle['sources']['excerpts']['text'])
        self.assertEqual(excerpts['excerpts'][0]['path'], 'src/code.rs')
        self.assertEqual(len(excerpts['excerpts'][0]['text'].splitlines()), 200)
        self.assertEqual(excerpts['excerpts'][0]['start'], 1)
        self.assertEqual(excerpts['missing_evidence'], [])
        self.assertIn('Fixed #432', bundle['sources']['citations']['text'])
        rubric = (ROOT / 'docs/design/issue-triage.md').read_text().split('## 4. Labels', 1)[1].split('## 6.', 1)[0]
        self.assertEqual(bundle['sources']['rubric']['text'], '## 4. Labels' + rubric)

    def test_manifest_never_reads_a_worktree_or_issue_controlled_path(self):
        real = Path.read_text
        def no_worktree(path, *args, **kwargs):
            self.assertFalse(str(path).startswith(str(self.repo)))
            return real(path, *args, **kwargs)
        with mock.patch.object(Path, 'read_text', no_worktree):
            self.assertIn('line 3', self.manifest.text('src/code.rs'))

    def test_existing_line_numbers_use_kernel_lf_crlf_semantics_not_unicode_splitlines(self):
        self.assertEqual(R.repo_lines('a\x85b\u2028c'), ['a\x85b\u2028c'])
        self.assertEqual(R.repo_lines('a\r\nb\r'), ['a', 'b\r'])
        self.assertEqual(R.repo_lines('a\n'), ['a'])
        self.assertEqual(R.repo_lines(''), [])
        result = json.loads(self.bundle(issue(body='src/unicode_lines:2'))['sources']['excerpts']['text'])
        self.assertEqual(result['excerpts'], [])
        self.assertIn('existing line', result['missing_evidence'][0]['reason'])
        result = json.loads(self.bundle(issue(body='src/crlf:2'))['sources']['excerpts']['text'])
        self.assertEqual(result['missing_evidence'], [])
        self.assertEqual(result['excerpts'][0]['text'], 'a\nb\r')

    def test_replace_refs_cannot_change_excerpts_from_the_captured_manifest(self):
        original = self.manifest.entries['src/code.rs'][2]
        replacement = self.processes.run(fixture_git_argv(self.repo, 'hash-object', '-w', '--stdin'),
                                         data=b'forged replacement\n', env=fixture_git_environment()).decode().strip()
        self.processes.run(fixture_git_argv(self.repo, 'replace', original, replacement),
                           env=fixture_git_environment())
        self.addCleanup(lambda: self.processes.run(fixture_git_argv(self.repo, 'replace', '-d', original),
                                                 env=fixture_git_environment()))
        self.assertIn('line 3', self.manifest.text('src/code.rs'))
        self.assertNotIn('forged', self.bundle()['sources']['excerpts']['text'])

    def test_absolute_traversal_symlink_binary_urls_absent_noncanonical_and_lines_refused(self):
        paths = ['/etc/passwd:1', '../secret:1', 'src/../code.rs:1', './src/code.rs:1',
                 'src//code.rs:1', 'src/link:1', 'src/binary:1', 'https://example.invalid/src/code.rs:1',
                 'missing:1', 'src/code.rs:0', 'src/code.rs:-1', 'src/code.rs:401',
                 'src/gitlink:1', 'src/nontext:1', 'src/untracked:1', 'src/50%-off:1',
                 '/docs/a b.md:1', '../docs/a b.md:1', 'docs/../docs/a b.md:1',
                 'https://example.invalid/docs/a b.md:1', './docs/a b.md:1', 'docs//a b.md:1']
        for path in paths:
            with self.subTest(path=path):
                bundle = self.bundle(issue(body=path))
                receipt = json.loads(bundle['sources']['excerpts']['text'])
                self.assertEqual(receipt['excerpts'], [])
                self.assertEqual(len(receipt['missing_evidence']), 1)

    def test_canonical_percent_and_unicode_manifest_spelling_matches_kernel(self):
        for raw, expected in ((b'docs/50%-off.md', 'docs/50%25-off.md'),
                              (b'a\xffb', 'a%FFb'), (b' notes.md', '%20notes.md'), (b'\x1cnotes.md', '\x1cnotes.md'),
                              ('src/кириллица'.encode(), 'src/кириллица')):
            self.assertEqual(R.manifest_path(raw), expected)
        self.assertEqual(self.manifest.text('src/50%25-off'), 'percent\n')
        with self.assertRaises(R.Refused):
            self.manifest.text('src/50%-off')

    def test_excerpts_count_and_aggregate_byte_bounds_record_missing_evidence(self):
        bundle = self.bundle(issue(body=' '.join(['src/code.rs:1'] * 18)))
        result = json.loads(bundle['sources']['excerpts']['text'])
        self.assertEqual(len(result['excerpts']), 16)
        self.assertEqual(len(result['missing_evidence']), 2)
        with mock.patch.object(self.manifest, 'text', side_effect=lambda p: 'x' * 150000 if p == 'huge' else
                               (ROOT / 'docs/design/issue-triage.md').read_text()), \
                mock.patch.dict(self.manifest.entries, {'huge': ('100644', 'blob', 'fake')}):
            result = json.loads(self.bundle(issue(body='huge:1'))['sources']['excerpts']['text'])
            self.assertIn('byte bound', result['missing_evidence'][0]['reason'])


class SourceAssembly(Fixture):
    def test_source_entry_count_entry_total_and_file_bounds_have_durable_receipts(self):
        bundle = R.bound_sources({'s%d' % n: 'x' for n in range(300)})
        self.assertEqual(len(bundle['sources']), 256)
        self.assertIn('entry-count', bundle['sources']['assembly']['text'])
        bundle = R.bound_sources({'a': 'x' * (R.ENTRY_MAX + 1), 'b': 'y' * R.ENTRY_MAX, 'c': 'z' * R.ENTRY_MAX})
        R.validate_sources(bundle)
        self.assertIn('truncat', bundle['sources']['assembly']['text'])
        self.assertTrue(all(len(v['text'].encode()) <= R.ENTRY_MAX for v in bundle['sources'].values()))
        self.assertLessEqual(sum(len(v['text'].encode()) for v in bundle['sources'].values()), R.TOTAL_MAX)
        bundle = R.bound_sources({'a': '\x01' * R.ENTRY_MAX, 'b': '\x02' * R.ENTRY_MAX})
        self.assertLessEqual(len(R.encoded(bundle)), R.FILE_MAX)
        self.assertIn('file-bound', bundle['sources']['assembly']['text'])

    def test_generation_and_bundle_identity_are_deterministic_and_full_input_sensitive(self):
        one = self.bundle()
        self.assertEqual(one, self.bundle())
        identity = R.generation_identity(432, one, self.main, 'event')
        self.assertEqual(R.digest(identity), R.digest(R.generation_identity(432, self.bundle(), self.main, 'event')))
        for changed in (self.bundle(issue(body='different')), self.bundle(issue(comments=[dict(id=1, body='reply')]))):
            self.assertNotEqual(R.digest(one), R.digest(changed))
        for field in identity:
            other = dict(identity, **{field: 'different'})
            self.assertNotEqual(R.digest(identity), R.digest(other))
        # A change beyond the retained text still changes the entry revision and generation.
        a = R.bound_sources({'a': 'x' * 300000 + 'a'})
        b = R.bound_sources({'a': 'x' * 300000 + 'b'})
        self.assertNotEqual(R.digest(a), R.digest(b))

    def test_nearest_ten_titles_and_open_merged_pull_closing_references(self):
        pulls = [dict(number=1, title='Fix #432', body='', merged=False, on_main=False, closing=[]),
                 dict(number=2, title='Fix', body='', merged=True, on_main=True, closing=[432]),
                 dict(number=3, title='Other #4320', body='', merged=True, on_main=True, closing=[])]
        neighbours = [dict(number=n, title='Issue %d' % n) for n in range(20)]
        bundle = R.assemble(self.manifest, issue(), pulls, neighbours)
        self.assertEqual(len(json.loads(bundle['sources']['neighbours']['text'])), 10)
        self.assertEqual([p['number'] for p in json.loads(bundle['sources']['pulls']['text'])], [1, 2])


class StateAndAdmission(Fixture):
    def test_claim_precedes_planning_and_reservation_precedes_admission(self):
        self.queue()
        self.assertEqual(self.tasks.calls, 1)
        key, gen = self.gen()
        self.assertEqual(self.journal.data['ledger'][key]['settled'], 1000)
        self.assertEqual(gen['status'], 'published')

    def test_journal_atomic_roundtrip_schema_parity_and_old_shape_fail_closed(self):
        self.queue()
        doc = R.strict_json(self.journal.path.read_bytes())
        schema = C.Validator(ROOT / 'schemas/issue-triage-runner-v3.json')
        self.assertEqual(schema.errors(doc, 'urn:af:schema:issue-triage-runner:3'), [])
        broken = copy.deepcopy(doc)
        broken['schema'] = 'af.issue-triage-runner/0'
        self.assertTrue(schema.errors(broken, 'urn:af:schema:issue-triage-runner:3'))
        broken = copy.deepcopy(doc)
        broken['ledger'][next(iter(broken['ledger']))]['reserved'] = 1
        self.assertTrue(schema.errors(broken, 'urn:af:schema:issue-triage-runner:3'))
        broken = copy.deepcopy(doc)
        del broken['issues']['432']['aux_labels']
        self.assertTrue(schema.errors(broken, 'urn:af:schema:issue-triage-runner:3'))
        broken = copy.deepcopy(doc)
        broken['issues']['432']['aux_labels']['evidence'] = [False]
        self.assertTrue(schema.errors(broken, 'urn:af:schema:issue-triage-runner:3'))
        self.assertFalse(list(self.root.glob('.write-*')))
        with mock.patch.object(R.os, 'replace', side_effect=OSError('crash before replace')):
            with self.assertRaises(OSError):
                self.journal.save()
        self.assertEqual(R.strict_json(self.journal.path.read_bytes()), doc)

    def test_journal_rejects_symlink_public_root_and_duplicate_json(self):
        with self.assertRaises(R.Refused):
            R.strict_json('{"schema":1,"schema":2}')
        public = self.root / 'public'
        public.mkdir(mode=0o755)
        with self.assertRaises(R.Refused):
            R.safe_root(public)
        linked = self.root / 'link'
        linked.symlink_to(public)
        with self.assertRaises(R.Refused):
            R.safe_root(linked)

    def claim_only(self, name):
        row = issue()
        return self.journal.claim(R.generation_identity(432, self.bundle(row), self.main, name), self.bundle(row), row)[0]

    def test_daily_cap_includes_settled_outstanding_and_new_reservation(self):
        keys = [self.claim_only(str(n)) for n in range(14)]
        for key in keys[:13]:
            self.journal.reserve(key, NOW)
        with self.assertRaisesRegex(R.Refused, 'daily token cap'):
            self.journal.reserve(keys[13], NOW)
        self.journal.settle(keys[0], dict(chargeable_tokens='50000', phase=dict(kind='finished')))
        self.journal.reserve(keys[13], NOW)
        self.assertEqual(sum(r['settled'] if r['settled'] is not None else r['reserved']
                             for r in self.journal.data['ledger'].values()), R.DAILY)

    def test_unknown_usage_never_releases_reservation_even_across_local_midnight(self):
        key = self.claim_only('unknown')
        self.journal.reserve(key, NOW)
        for show in ({}, dict(chargeable_tokens='100', unknown_usage_attempts=1, phase=dict(kind='finished')),
                     dict(chargeable_tokens='-1'), dict(chargeable_tokens=0), dict(chargeable_tokens='0', unknown_usage_attempts=True)):
            self.journal.settle(key, show)
            self.assertIsNone(self.journal.data['ledger'][key]['settled'])
        self.assertEqual(self.journal.data['ledger'][key]['day'], '2026-11-02')
        for n in range(12):
            self.journal.reserve(self.claim_only('later%d' % n), NOW + dt.timedelta(days=1))
        with self.assertRaises(R.Refused):
            self.journal.reserve(self.claim_only('over'), NOW + dt.timedelta(days=1))

    def test_timezone_day_is_claw_day_and_cannot_change_existing_ledger_identity(self):
        key = self.claim_only('tz')
        self.journal.reserve(key, dt.datetime(2026, 11, 1, 22, tzinfo=dt.timezone.utc))
        self.assertEqual(self.journal.data['ledger'][key]['day'], '2026-11-02')
        self.journal.close()
        with self.assertRaises(R.Refused):
            R.Journal(self.root, 'owner/repo', 'runner', 'UTC')

    def test_unknown_usage_and_lost_admission_have_no_public_effect(self):
        self.tasks.unknown = True
        self.queue()
        self.assertEqual(self.api.writes, [])
        key, gen = self.gen()
        self.assertEqual(gen['status'], 'uncertain')
        self.runner.run_generation(key, gen)
        self.assertEqual(self.tasks.calls, 1)
        self.assertEqual(self.api.writes, [])

    def test_third_failed_generation_sets_needs_info_without_guessed_classification(self):
        self.tasks.failure = True
        for n in range(3):
            self.queue('failure%d' % n)
            if n < 2:
                self.assertEqual(self.api.writes, [])
        self.assertIn('triage/needs-info', self.api.rows[432]['labels'])
        self.assertNotIn('class/bug', self.api.rows[432]['labels'])
        self.assertIn('three failed generations', self.api.rows[432]['comments'][0]['body'])
        self.assertEqual(self.journal.issue(432)['failures'], 3)

    def test_four_issue_and_batch_deadline_bounds(self):
        self.api.rows = {n: issue(n) for n in range(1, 7)}
        self.runner.tick(time.monotonic() + 5400)
        self.assertEqual(self.tasks.calls, 4)
        self.assertEqual(len({w[1] for w in self.api.writes}), 4)
        self.runner.tick(time.monotonic() + 100)
        self.assertEqual(self.tasks.calls, 4)

    def test_busy_or_uncertain_shared_lease_defers_before_any_work(self):
        with mock.patch.object(R.host_lock, 'read_anchor', return_value=[1, 2]), \
                mock.patch.object(R.host_lock, 'MachineLock', side_effect=R.host_lock.Refused('concurrent_shard', 'busy')), \
                mock.patch.object(R, 'Journal') as journal, mock.patch.object(R, 'GitHub') as api:
            code = R.main(['--repo', str(self.repo), '--state-root', str(self.root), '--principal', 'runner',
                           '--github-token-file', str(self.root / 'absent-token')])
            self.assertEqual(code, 1)
            journal.assert_not_called()
            api.assert_not_called()
        self.lease.valid = False
        with self.assertRaises(R.Refused):
            self.queue()
        self.assertEqual(self.tasks.calls, 0)
        self.assertEqual(self.api.writes, [])
        self.lease.valid = True

    def test_process_output_bound_cleanup_and_group_uncertainty_retain_fence(self):
        out_read, out_write = os.pipe()
        err_read, err_write = os.pipe()
        os.write(out_write, b'x' * 200)
        os.close(out_write)
        os.close(err_write)
        exited = mock.Mock(pid=99999998, returncode=0)
        exited.poll.return_value = 0
        exited.stdout, exited.stderr = os.fdopen(out_read, 'rb'), os.fdopen(err_read, 'rb')
        with mock.patch.object(R, 'OUTPUT_MAX', 100), \
                mock.patch.object(R.subprocess, 'Popen', return_value=exited), \
                mock.patch.object(R.os, 'killpg', side_effect=ProcessLookupError):
            with self.assertRaisesRegex(R.Refused, 'output bound'):
                self.processes.run([str(self.root / 'fake-af')])
        self.assertFalse(self.processes.children)
        self.assertTrue(self.lease.uncertain)
        self.lease.uncertain = False
        proc = mock.Mock(pid=99999999)
        with mock.patch.object(R.os, 'killpg', return_value=None):
            with self.assertRaisesRegex(R.Refused, 'cleanup uncertain'):
                self.processes.stop(proc)
        self.assertTrue(self.lease.uncertain)
        with self.assertRaises(R.Refused):
            self.lease.confirm_clean()
        self.lease.uncertain = False


class EventsAndPublication(Fixture):
    def test_every_trigger_consumed_once_and_cursor_enqueued_atomically(self):
        self.journal.issue(432)['needs_since'] = (NOW - dt.timedelta(hours=1)).isoformat()
        row = issue(labels=['triage/needs-info', 'triage/pending'], comments=[
            dict(id=1, author='reporter', body='/triage', created_at=NOW.isoformat()),
            dict(id=2, author='agent', body='/triage size-wrong too large', created_at=NOW.isoformat()),
            dict(id=3, author='reporter', body='Here is the repro', created_at=NOW.isoformat()),
            dict(id=4, author='runner', body='canonical', created_at=NOW.isoformat())],
                    events=[dict(id=5, actor='owner', event='labeled', label='size/L', created_at=NOW.isoformat()),
                            dict(id=6, actor='reporter', event='labeled', label='triage/pending', created_at=NOW.isoformat()),
                            dict(id=7, actor='owner', event='unlabeled', label='size/M', created_at=NOW.isoformat())])
        R.discover(self.journal, [row], 'runner', 'owner')
        self.assertEqual(len(self.journal.data['events']), 7)
        R.discover(self.journal, [row], 'runner', 'owner')
        self.assertEqual(len(self.journal.data['events']), 7)
        row['body'] += ' edit'
        R.discover(self.journal, [row], 'runner', 'owner')
        self.assertEqual(len(self.journal.data['events']), 8)
        self.assertEqual(self.journal.data['cursor'], row['updated_at'])
        self.assertEqual(len(R.strict_json(self.journal.path.read_bytes())['events']), 8)

    def test_labels_preserve_unrelated_unknown_namespaces_and_protected_decision(self):
        current = ['test:perf', 'release:minor', 'no-perf', 'area/new', 'class/future', 'outside',
                   'class/decision', 'size/L', 'pipeline/design', 'triage/pending', 'bug', 'enhancement']
        desired = R.labels_for(current, dict(labels=['class/decision', 'size/M', 'pipeline/hand']))
        self.assertEqual(desired, sorted(['test:perf', 'release:minor', 'no-perf', 'area/new', 'class/future',
                                         'outside', 'class/decision', 'size/M', 'pipeline/hand']))
        self.assertTrue(R.PROTECTED <= set(R.labels_for(list(R.PROTECTED), dict(labels=[]))))

    def test_idempotent_labels_one_canonical_comment_and_journal_consumer_freshness(self):
        key = self.queue()
        gen = self.journal.data['generations'][key]
        writes = copy.deepcopy(self.api.writes)
        self.runner.publish(key, gen)
        self.assertEqual(self.api.writes, writes)
        self.assertEqual(len(self.api.rows[432]['comments']), 1)
        comment = copy.deepcopy(self.api.rows[432]['comments'][0])
        self.assertEqual(R.consume_comment(comment, 432, self.journal), gen['record'])
        for field, value in (('id', 99), ('author', 'attacker'), ('body', comment['body'].replace(key, '0' * 64))):
            with self.assertRaises((R.Refused, C.Refusal)):
                R.consume_comment(dict(comment, **{field: value}), 432, self.journal)
        self.api.rows[432]['body'] += ' new evidence'
        next_key = self.queue('edited')
        self.assertNotEqual(next_key, key)
        self.assertEqual(len(self.api.rows[432]['comments']), 1)
        self.assertEqual(self.journal.issue(432)['comment_id'], comment['id'])
        self.assertEqual(self.api.writes[-1][3], comment['id'])
        with self.assertRaises(R.Refused):
            R.consume_comment(comment, 432, self.journal)

    def test_owner_override_kept_in_labels_and_merged_json_and_conflicts_suspend(self):
        self.api.rows[432].update(labels=['outside', 'size/L', 'pipeline/hand'], events=[
            dict(id=1, actor='owner', event='labeled', label='size/L', created_at=NOW.isoformat()),
            dict(id=2, actor='owner', event='labeled', label='pipeline/hand', created_at=NOW.isoformat())])
        self.queue()
        _, gen = self.gen()
        self.assertEqual(gen['record']['size'], 'L')
        self.assertEqual(gen['record']['pipeline'], 'hand')
        self.assertEqual(gen['record']['owner_override'], dict(size='size/L', pipeline='pipeline/hand'))
        self.api.rows[432]['labels'].append('size/M')
        self.api.rows[432]['events'].append(dict(id=100, actor='owner', event='labeled', label='size/M', created_at=NOW.isoformat()))
        before = copy.deepcopy(self.api.writes)
        with self.assertRaises(C.Refusal):
            self.queue('conflicting-owner')
        self.assertEqual(self.api.writes, before)
        self.assertEqual(self.tasks.calls, 1)

    def test_owner_unlabel_removes_override_provenance(self):
        row = issue(labels=['size/L'], events=[
            dict(id=1, actor='owner', event='labeled', label='size/L'),
            dict(id=2, actor='owner', event='unlabeled', label='size/L'),
            dict(id=3, actor='other', event='labeled', label='size/L')])
        self.assertEqual(R.owner_labels(row, 'owner'), [])

    def test_failed_disagreement_publishes_disputed_canonical_record_with_owner_override(self):
        self.api.rows[432].update(labels=['outside', 'class/uix'], events=[
            dict(id=1, actor='owner', event='labeled', label='class/uix', created_at=NOW.isoformat())])
        collect = self.tasks.collect
        def differing_result(*args):
            result = collect(*args)
            verifier = dict(result['author'], **{'class': 'aix'})
            result['evaluation'] = HELPERS.evaluation('failed', verifier)
            return result
        with mock.patch.object(self.tasks, 'collect', side_effect=differing_result):
            key = self.queue()
        gen = self.journal.data['generations'][key]
        self.assertEqual(gen['status'], 'published')
        comment, = self.api.rows[432]['comments']
        record = R.consume_comment(comment, 432, self.journal)
        self.assertEqual(record, gen['record'])
        self.assertEqual(record['verdict'], 'disputed')
        self.assertEqual(record['class'], 'uix')
        self.assertEqual(record['owner_override'], {'class': 'class/uix'})
        self.assertEqual((record['author']['class'], record['verifier']['class']), ('bug', 'aix'))
        self.assertEqual(record['labels'], ['class/uix', 'triage/disputed'])
        self.assertEqual(self.api.rows[432]['labels'], ['class/uix', 'outside', 'triage/disputed'])
        self.assertNotIn('"verdict": "agreed"', comment['body'])
        self.assertEqual(self.tasks.calls, 1)

    def test_nonempty_needs_info_from_either_triager_blocks_agreement(self):
        self.tasks.needs_info = ['What is the measured result?']
        self.queue()
        _, gen = self.gen()
        self.assertEqual(gen['record']['verdict'], 'needs-info')
        self.assertIn('triage/needs-info', self.api.rows[432]['labels'])
        self.assertNotIn('class/bug', self.api.rows[432]['labels'])
        self.assertIn('What is the measured result?', gen['comment'])
        # The accepted check also blocks an author's question independently.
        a = HELPERS.doc(needs_info=['Repro?'])
        b = HELPERS.doc()
        self.assertEqual(C.merge(a, b)['verdict'], 'needs-info')

    def test_stale_body_comments_labels_neighbours_pulls_or_main_never_publish_old_generation(self):
        changes = [lambda: self.api.rows[432].update(body='body changed'),
                   lambda: self.api.rows[432]['comments'].append(dict(id=4, author='reporter', body='reply', created_at=NOW.isoformat())),
                   lambda: self.api.rows[432]['labels'].append('unexpected'),
                   lambda: self.api.rows.update({433: issue(433)}),
                   lambda: self.api.pull_rows.append(dict(number=9, title='Related #432', body='', closing=[], merged=False, on_main=False))]
        for index, change in enumerate(changes):
            with self.subTest(change=index):
                self.api.rows = {432: issue()}
                self.api.pull_rows = []
                self.tasks.change = change
                key = self.queue('stale%d' % index)
                self.assertEqual(self.journal.data['generations'][key]['status'], 'stale')
                self.assertEqual(self.api.writes, [])
        self.assertTrue(any(k.startswith('stale:') for k in self.journal.data['events']))
        self.tasks.change = lambda: setattr(self.manifest, 'main', 'f' * 40)
        self.api.rows = {432: issue()}
        self.api.pull_rows = []
        key = self.queue('main-moved')
        self.assertEqual(self.journal.data['generations'][key]['status'], 'stale')
        self.assertEqual(self.api.writes, [])

    def test_crash_after_generation_claim_does_not_duplicate_task_or_generation(self):
        with mock.patch.object(self.tasks, 'execute', side_effect=KeyboardInterrupt):
            with self.assertRaises(KeyboardInterrupt):
                self.queue()
        key, gen = self.gen()
        self.assertEqual(gen['status'], 'claimed')
        self.assertEqual(self.journal.data['events']['opened']['generation'], key)
        self.runner.run_generation(key, gen)
        self.assertEqual(self.tasks.calls, 1)
        self.assertEqual(len(self.journal.data['generations']), 1)

    def test_ambiguous_labels_reconcile_before_comment_with_separate_checkpoints(self):
        self.api.fail_labels = True
        with self.assertRaises(R.Ambiguous):
            self.queue()
        key, gen = self.gen()
        self.assertFalse(gen['labels_done'])
        self.assertFalse(gen['comment_done'])
        self.assertTrue(gen['labels_intent'])
        self.runner.run_generation(key, gen)
        self.assertEqual(gen['status'], 'published')
        self.assertEqual(len(self.api.rows[432]['comments']), 1)
        self.assertEqual(self.tasks.calls, 1)

    def test_ambiguous_comment_create_reconciles_marker_and_never_duplicates(self):
        self.api.fail_comment = True
        with self.assertRaises(R.Ambiguous):
            self.queue()
        key, gen = self.gen()
        self.assertTrue(gen['labels_done'])
        self.assertFalse(gen['comment_done'])
        self.assertIsNone(self.journal.issue(432)['comment_id'])
        self.runner.run_generation(key, gen)
        self.assertEqual(gen['status'], 'published')
        self.assertEqual(len(self.api.rows[432]['comments']), 1)
        self.assertEqual(len([w for w in self.api.writes if w[0] == 'comment']), 1)

    def test_ambiguous_create_without_observable_marker_suspends_instead_of_retry(self):
        self.api.fail_before_comment = True
        with self.assertRaises(R.Ambiguous):
            self.queue()
        key, gen = self.gen()
        with self.assertRaises(R.Ambiguous):
            self.runner.run_generation(key, gen)
        self.assertEqual(len([w for w in self.api.writes if w[0] == 'comment']), 1)
        self.assertNotEqual(gen['status'], 'published')

    def test_ambiguous_patch_uses_recorded_comment_id_and_does_not_duplicate(self):
        self.queue()
        self.api.rows[432]['body'] += ' edit'
        self.api.fail_comment = True
        with self.assertRaises(R.Ambiguous):
            self.queue('edit')
        key = self.journal.data['events']['edit']['generation']
        gen = self.journal.data['generations'][key]
        self.runner.run_generation(key, gen)
        self.assertEqual(len(self.api.rows[432]['comments']), 1)
        self.assertEqual(gen['status'], 'published')

    def test_unjournaled_or_multiple_canonical_comments_suspend_writeback(self):
        self.runner.apply = False
        key = self.queue()
        gen = self.journal.data['generations'][key]
        self.runner.apply = True
        self.api.rows[432]['comments'] = [dict(id=44, author='runner', body=gen['comment'].replace(key, 'a' * 64), created_at=NOW.isoformat())]
        with self.assertRaises(R.Refused):
            self.runner.publish(key, gen)
        self.assertEqual(self.api.writes, [])
        self.api.rows[432]['comments'] = [dict(id=n, author='runner', body=gen['comment'], created_at=NOW.isoformat()) for n in (44, 45)]
        with self.assertRaises(R.Refused):
            self.runner.publish(key, gen)
        self.assertEqual(self.api.writes, [])

    def test_dry_run_prints_exact_labels_comment_and_has_zero_mutations(self):
        self.runner.apply = False
        key = self.queue()
        gen = self.journal.data['generations'][key]
        self.assertEqual(self.api.writes, [])
        self.assertEqual(self.notifications, [])
        self.assertIn(json.dumps(gen['desired_labels']), self.output[0])
        self.assertIn(gen['comment'], self.output[0])
        self.assertEqual(gen['status'], 'dry-run')

    def test_existing_security_or_held_and_either_security_triager_are_private_only(self):
        for label in ('class/security', 'triage/held'):
            self.api.rows[432]['labels'].append(label)
            self.queue('hold-' + label)
            self.assertEqual(self.api.writes, [])
            self.assertEqual(self.tasks.calls, 0)
            self.api.rows[432]['labels'].remove(label)
        self.assertEqual(self.notifications, [432])
        for who in ('author', 'verifier'):
            self.journal.issue(432).update(hold=None, notified=False)
            self.tasks.security = who
            self.queue('security-' + who)
            self.assertEqual(self.api.writes, [])
            self.assertEqual(self.journal.issue(432)['hold']['security_triagers'], [who])
        self.assertEqual(self.output, [])

    def test_hold_skipped_until_owner_release_and_notification_failure_stays_private(self):
        self.tasks.security = 'author'
        self.runner.notifier = lambda _: False
        self.queue()
        self.assertFalse(self.journal.issue(432)['notified'])
        self.assertIsNotNone(self.journal.issue(432)['hold'])
        self.assertEqual(self.api.writes, [])
        self.tasks.security = None
        self.runner.notifier = lambda n: self.notifications.append(n) or True
        self.queue('held-again')
        self.assertEqual(self.tasks.calls, 1)
        self.clock = NOW + dt.timedelta(minutes=1)
        self.api.rows[432]['events'].append(dict(id=20, actor='owner', event='unlabeled', label='triage/held', created_at=self.clock.isoformat()))
        self.queue('released')
        self.assertEqual(self.tasks.calls, 2)
        self.assertTrue(self.api.writes)

    def test_lease_loss_after_task_or_before_write_has_no_public_effect(self):
        self.tasks.change = lambda: setattr(self.lease, 'valid', False)
        with self.assertRaises(R.Refused):
            self.queue()
        self.assertEqual(self.api.writes, [])
        self.lease.valid = True


class Sweep(Fixture):
    def test_contextual_completed_fix_mentions_produce_only_owner_review(self):
        for text in ('Fixed unrelated bug, see #432 for context',
                     'Reverted the change that fixed #432.',
                     'Fixed unrelated bug, related to #432.',
                     'Fixed a bug described in #432.',
                     '#432 tracks another bug, now fixed elsewhere.',
                     'Fixes #4320; see #432 for context.',
                     'Shipped fix for #4320; see #432 for context.',
                     'Shipped unrelated fix, see #432 for context.',
                     'Not shipped fix for #432.', 'Partially shipped fix for #432.',
                     'Shipped fix for #432, but remaining items follow.',
                     "Fixed #432's duplicate, #999.", 'Fixed #432-related issue #999.',
                     'Fixed #999 and see #432 for context.'):
            with self.subTest(text=text):
                cite = dict(path='CHANGELOG.md', line=1, start=1, text=text)
                with mock.patch.object(self.manifest, 'citations', return_value=[cite]):
                    self.runner.sweep()
                self.assertEqual(self.api.writes, [])
                self.assertNotIn('triage/maybe-fixed', self.api.rows[432]['labels'])
                self.assertEqual(self.api.rows[432]['comments'], [])
                self.assertEqual(self.api.rows[432]['state'], 'open')
                self.assertTrue(any(text in entry['citation'] for entry in self.journal.data['owner_review'].values()))
        self.assertEqual(self.tasks.calls, 0)

    def test_explicit_completed_fixes_vs_bare_negated_partial_and_future_mentions(self):
        def cite(text):
            return dict(path='CHANGELOG.md', line=1, start=1, text=text)
        for text in ('Fixed #432: correct grouping.', '- Fixed #432: correct grouping.',
                     '* Shipped a fix for issue #432.', 'Other changes. Fixed #432.',
                     '- Other changes\n- Fixed #432.', 'Shipped a fix\nfor issue #432.',
                     'Changes; #432 has been completed.', 'Implemented #432.', 'Resolved #432.',
                     'Fixes #432.', 'Resolves issue #432.', 'Completed issue #432.',
                     'Shipped fix for #432.', 'Shipped a fix for issue #432.',
                     'Implemented the fix for #432.',
                     '#432 is fixed.', '#432 was resolved.', '#432 has been completed.'):
            positive, review = R.sweep_evidence([cite(text)], [], 432)
            self.assertEqual(len(positive), 1)
            self.assertEqual(review, [])
        for text in ('See #432.', '#432 is not done here.', 'Partially fixed #432.',
                     'Fixed one item of #432; remaining items follow.', 'Will be fixed #432.',
                     "#432 isn't fixed.", 'Not completed: #432.', 'Fixed #432 parsing; authentication still needs work.',
                     'Fixed one part of #432.', 'Implemented #432 phase one.'):
            with self.subTest(text=text):
                positive, review = R.sweep_evidence([cite(text)], [], 432)
                self.assertEqual(positive, [])
                self.assertEqual(len(review), 1)
        positive, _ = R.sweep_evidence([], [dict(number=5, merged=True, on_main=True, closing=[432])], 432)
        self.assertEqual(positive[0]['pull'], 5)
        self.assertEqual(R.sweep_evidence([], [dict(number=5, merged=False, on_main=False, closing=[432])], 432)[0], [])

    def test_complete_completion_clause_grammar(self):
        cases = (
            ('Fixed #432', True),
            ('Fixed #432.', True),
            ('Fixed #432; other changes.', True),
            ('Fixed #432: correct grouping.', True),
            ('Status: Fixed issue #432.', True),
            ('* Status: Shipped a fix for #432.', True),
            ('1) Status: #432 has been completed.', True),
            ('Status:\n#432 has been completed.', True),
            ('Fixed #432?', False),
            ('Fixed #432 was an incorrect claim.', False),
            ('Fixed #432 allegedly.', False),
            ('#432 has been completed according to the report.', False),
            ('Fixed #432: correct grouping?', False),
            ('Fixed #432, correct grouping.', False),
            ('The report says: #432 has been completed.', False),
            ('The report says: Status: #432 has been completed.', False),
            ('"Status: #432 has been completed."', False),
            ('- The change that\nfixed #432.', False),
        )
        cases += tuple((text, False) for text in CONTEXT_REFUSALS)
        for text, expected in cases:
            with self.subTest(text=text):
                positive, review = R.sweep_evidence(
                    [dict(path='CHANGELOG.md', line=1, start=1, text=text)], [], 432)
                self.assertEqual(len(positive), int(expected))
                self.assertEqual(len(review), int(not expected))

    def test_shipped_fix_for_issue_sets_label_and_citation_once_without_tasks_or_closure(self):
        cite = dict(path='changelog.d/fix.md', line=1, start=1, text='Shipped fix for #432.')
        with mock.patch.object(self.manifest, 'citations', return_value=[cite]):
            self.runner.sweep()
            self.runner.sweep()
        self.assertEqual(self.tasks.calls, 0)
        self.assertIn('triage/maybe-fixed', self.api.rows[432]['labels'])
        self.assertIn('outside', self.api.rows[432]['labels'])
        comments = self.api.rows[432]['comments']
        self.assertEqual(len(comments), 1)
        self.assertIn('changelog.d/fix.md:1', comments[0]['body'])
        self.assertIn('Shipped fix for #432.', comments[0]['body'])
        self.assertEqual(self.api.rows[432]['state'], 'open')

    def test_sweep_token_free_preserves_labels_no_close_and_evidence_dedup(self):
        self.runner.sweep()
        self.assertEqual(self.tasks.calls, 0)
        self.assertIn('triage/maybe-fixed', self.api.rows[432]['labels'])
        self.assertIn('outside', self.api.rows[432]['labels'])
        self.assertEqual(len(self.api.rows[432]['comments']), 1)
        self.runner.sweep()
        self.assertEqual(len(self.api.rows[432]['comments']), 1)
        self.assertEqual(self.api.rows[432]['state'], 'open')

    def test_sweep_preserves_needs_info_disputed_held_and_security(self):
        for label in ('triage/needs-info', 'triage/disputed', 'triage/held', 'class/security'):
            self.api.rows[432]['labels'] = [label, 'outside']
            self.runner.sweep()
            self.assertEqual(self.api.writes, [])
        self.assertEqual(self.tasks.calls, 0)

    def test_sweep_ambiguous_comment_reconciles_evidence_marker_without_duplicate(self):
        self.api.fail_comment = True
        with self.assertRaises(R.Ambiguous):
            self.runner.sweep()
        self.runner.sweep()
        self.assertEqual(len(self.api.rows[432]['comments']), 1)
        self.assertEqual(len([w for w in self.api.writes if w[0] == 'comment']), 1)

    def test_bare_mentions_go_only_to_owner_review_log(self):
        self.api.rows = {433: issue(433)}
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        self.assertTrue(self.journal.data['owner_review'])

    def test_one_needs_info_reminder_only_after_30_days_without_reply(self):
        self.api.rows[432]['labels'] = ['triage/needs-info']
        self.api.rows[432]['events'] = [dict(id=1, actor='runner', event='labeled', label='triage/needs-info', created_at=NOW.isoformat())]
        state = self.journal.issue(432)
        state['needs_since'] = NOW.isoformat()
        self.clock = NOW + dt.timedelta(days=29, hours=23)
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        self.clock = NOW + dt.timedelta(days=30)
        self.runner.sweep()
        self.assertEqual(len(self.api.rows[432]['comments']), 1)
        self.runner.sweep()
        self.assertEqual(len(self.api.rows[432]['comments']), 1)
        self.assertTrue(state['reminded'])

    def test_reply_suppresses_reminder_and_dry_sweep_has_no_writes(self):
        self.api.rows[432]['labels'] = ['triage/needs-info']
        self.api.rows[432]['events'] = [dict(id=1, actor='runner', event='labeled', label='triage/needs-info', created_at=NOW.isoformat())]
        state = self.journal.issue(432)
        state['needs_since'] = NOW.isoformat()
        self.clock = NOW + dt.timedelta(days=31)
        self.api.rows[432]['comments'].append(dict(id=1, author='reporter', body='Repro', created_at=(NOW + dt.timedelta(days=1)).isoformat()))
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        self.api.rows[432]['labels'] = []
        self.runner.apply = False
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        self.assertTrue(self.output)


class RealAdapter(Fixture):
    def test_strict_since_overlap_discovers_late_same_second_issue_once(self):
        github = R.GitHub('owner/repo', 'fake-token', self.lease, time.monotonic() + 5400, 'runner')
        visible, queries = [432], []
        stamp = '2026-11-02T01:00:00Z'
        self.api.rows[432]['updated_at'] = stamp
        self.api.rows[433] = issue(433, updated_at=stamp)
        def transport(method, path, **kwargs):
            self.assertEqual(method, 'GET')
            if path == '/user':
                return 200, dict(login='runner')
            query = parse_qs(urlsplit(path).query)
            self.assertEqual(urlsplit(path).path, '/repos/owner/repo/issues')
            self.assertEqual(query['page'], ['1'])
            queries.append(query)
            since = query.get('since', [''])[0]
            return 200, [copy.deepcopy(self.api.rows[n]) for n in visible
                         if not since or R.instant(self.api.rows[n]['updated_at']) > R.instant(since)]
        self.runner.api = github
        with mock.patch.object(github.rest, 'call', side_effect=transport), \
                mock.patch.object(github, 'issue', side_effect=self.api.issue):
            # Expired batch admission leaves discovery events queued, without starting Tasks.
            self.runner.tick(time.monotonic())
            self.assertNotIn('since', queries[0])
            first = copy.deepcopy(self.journal.data['events'])
            self.assertEqual(len(first), 1)
            self.assertEqual(self.journal.data['cursor'], stamp)
            visible.append(433)  # Becomes visible only after the cursor second was saved.
            self.runner.tick(time.monotonic())
            self.assertEqual(queries[-1]['since'], ['2026-11-02T00:59:59Z'])
            second = [key for key, event in self.journal.data['events'].items() if event['issue'] == 433]
            self.assertEqual(len(second), 1)
            self.assertEqual(self.journal.data['events'][second[0]]['status'], 'queued')
            self.journal.data['events'][second[0]]['status'] = 'claimed'
            self.journal.save()
            expected = copy.deepcopy(self.journal.data)
            self.assertEqual(C.Validator(ROOT / 'schemas/issue-triage-runner-v3.json').errors(
                expected, 'urn:af:schema:issue-triage-runner:3'), [])
            for _ in range(2):
                self.runner.tick(time.monotonic())
                self.assertEqual(self.journal.data, expected)
                self.assertEqual(R.strict_json(self.journal.path.read_bytes()), expected)
            self.assertEqual(len(self.journal.data['events']), 2)
            self.assertTrue(first.keys() <= self.journal.data['events'].keys())
        self.assertEqual(self.tasks.calls, 0)

    def test_cursor_overlap_normalizes_valid_timestamp_boundaries_and_omits_empty_cursor(self):
        github = R.GitHub('owner/repo', 'fake-token', self.lease, time.monotonic() + 5400, 'runner')
        for cursor, expected in (('', None),
                                 ('2026-01-01T00:00:00Z', '2025-12-31T23:59:59Z'),
                                 ('2026-01-01T03:00:00+03:00', '2025-12-31T23:59:59Z'),
                                 ('2026-11-02T01:00:00.500000Z', '2026-11-02T00:59:59.500000Z')):
            with self.subTest(cursor=cursor), mock.patch.object(github.rest, 'call', return_value=(200, [])) as call:
                self.assertEqual(github.open_issues(cursor), [])
                query = parse_qs(urlsplit(call.call_args.args[1]).query)
                self.assertEqual(query.get('since'), [expected] if expected else None)

    def test_discovery_pagination_failure_preserves_cursor_and_events_atomically(self):
        github = R.GitHub('owner/repo', 'fake-token', self.lease, time.monotonic() + 5400, 'runner')
        self.runner.api = github
        self.journal.data['cursor'] = NOW.isoformat()
        self.journal.save()
        before = self.journal.path.read_bytes()
        for bounded in (False, True):
            pages = []
            def transport(method, path, **kwargs):
                if path == '/user':
                    return 200, dict(login='runner')
                page = int(parse_qs(urlsplit(path).query)['page'][0])
                pages.append(page)
                if not bounded and page == 2:
                    raise R.ApiRefused('fixture read failed')
                return 200, [dict(number=n) for n in range(100)]
            with self.subTest(bounded=bounded), mock.patch.object(github.rest, 'call', side_effect=transport):
                with self.assertRaisesRegex(R.Refused, 'page bound|GitHub read failed'):
                    self.runner.tick(time.monotonic())
            self.assertEqual(pages, list(range(1, 21)) if bounded else [1, 2])
            self.assertEqual(self.journal.path.read_bytes(), before)
            self.assertEqual(self.journal.data, R.strict_json(before))
        self.assertEqual(self.tasks.calls, 0)

    def adapter(self):
        row = R.evidence_issue(issue(), 'runner', None)
        bundle = self.bundle(row)
        identity = R.generation_identity(432, bundle, self.main, 'adapter')
        key, gen = self.journal.claim(identity, bundle, row)
        task = R.Task(self.root, self.repo, self.processes, af=str(self.root / 'fake-af'))
        def put(value):
            return put_kernel(task, value)
        def envelope(kind, payload, **extras):
            return put(dict(type=kind, payload=payload, artifact_id='', content_id='', input_artifacts=[],
                            producer=dict(kind='kernel_operation', run_id='test-triage', operation_id='fixture'), **extras))
        origin = put(dict(schema='af.task-source-origin/3', kind='captured', source_revision=self.main,
                          repository_id='fixture', content_digest='sha256:' + 'a' * 64))
        source = put(dict(schema='af.task-snapshot/1', origin_id=origin))
        sources = envelope('af/ReportSources@1', bundle)
        author = HELPERS.doc(body_sha256=R.text_hash(row['body']))
        report = HELPERS.rendered(author)
        document = envelope('af/Document@1', dict(schema='af.document/1', text=report, sources_id=sources),
                            subject_snapshot_id=source)
        receipt = dict(outcome='passed', document_id=document, sources_id=sources, source_snapshot_id=source,
                       policy_id='sha256:' + '3' * 64, plan_id='sha256:' + 'a' * 64)
        check_id = envelope('af/ReportCheckReceipt@1', receipt)
        evaluation = dict(HELPERS.evaluation(block=author), document_id=document, sources_id=sources,
                          check_receipt_id=check_id, source_snapshot_id=source)
        evaluation_id = envelope('af/ReportEvaluation@1', evaluation)
        verification = dict(outcome='passed', document_id=document, source_snapshot_id=source,
                            check_receipt_id=check_id, evaluation_id=evaluation_id)
        verification_id = envelope('af/ReportVerification@1', verification)
        outputs = dict(report=dict(snapshot_id=source, artifact_ids=[document]),
                       verification=dict(snapshot_id=source, artifact_ids=[verification_id]))
        plan_id = 'sha256:' + 'a' * 64
        config = dict(key=key, task_id=gen['task_id'],
                      plan=dict(schema='af/task-inspection@20', task_id=gen['task_id'], attempts=0, plan_id=plan_id,
                                plan=dict(inputs=dict(source=dict(snapshot_id=source),
                                                      sources=dict(artifact_ids=[sources])))),
                      show=dict(schema='af/task-inspection@20', task_id=gen['task_id'], chargeable_tokens='1234',
                                phase=dict(kind='finished'), result=dict(outputs=outputs)), report=report)
        (self.root / 'cli.json').write_bytes(R.encoded(config))
        fake = self.root / 'fake-af'
        fake.write_text('''#!%s
import json
from pathlib import Path
import sys
root = Path(__file__).parent
config = json.loads((root / 'cli.json').read_text())
args = sys.argv[1:]
with (root / 'argv.log').open('a') as log:
    log.write(json.dumps(args) + '\\n')
assert args[0] == 'task'
command = args[1]
assert '--repo' in args and '--state' in args
assert args[args.index('--state') + 1] == str(root / 'task-store')
if command == 'plan':
    journal = json.loads((root / 'journal.json').read_text())
    assert journal['ledger'][config['key']]['reserved'] == 150000
    assert journal['generations'][config['key']]['status'] == 'planning'
    file = json.loads(Path(args[args.index('--file') + 1]).read_text())
    assert file['task_id'] == config['task_id']
    assert file['kind'] == 'triage' and file['pipeline'] == {'name':'backlog/triage','fallback':'refuse'}
    assert file['limits'] == {'tokens':150000,'max_attempts':8,'wall_ms':1200000,
                              'verification':{'tokens':72768,'attempts':3,'wall_ms':605000}}
    assert json.loads(Path(file['report_sources']).read_text()) == journal['generations'][config['key']]['bundle']
    print(json.dumps(config['plan']))
elif command == 'run':
    journal = json.loads((root / 'journal.json').read_text())
    assert journal['generations'][config['key']]['status'] == 'admitting'
    assert '--confirm-plan' in args and '--execute' not in args
    print(json.dumps(config['show']))
elif command == 'show':
    print(json.dumps(config['show']))
elif command == 'output':
    assert args[args.index('--port') + 1] == 'report'
    assert args[args.index('--format') + 1] == 'canonical-markdown'
    print(config['report'], end='')
else:
    raise AssertionError('unexpected command')
''' % sys.executable)
        fake.chmod(0o700)
        return key, gen, task, config, evaluation_id

    def test_real_task_argv_caps_source_evaluation_ids_and_check_integration(self):
        key, gen, task, config, _ = self.adapter()
        result = task.execute(gen, self.journal, key, NOW)
        record, code = C.check(result['author'], result['evaluation'], [],
                               R.Reach(self.manifest, set()), identity=dict(
                                   task_id=gen['task_id'], document_id=result['document_id'],
                                   evaluation_id=result['evaluation_id'], bundle_sha256=gen['identity']['bundle_sha256'],
                                   main=self.main, policy_version=R.POLICY, event_id='adapter'))
        self.assertEqual(code, 0)
        self.assertEqual(record['generation'], gen['identity'])
        self.assertEqual(record['sources']['evaluation_id'], result['evaluation_id'])
        self.assertEqual(self.journal.data['ledger'][key]['settled'], 1234)
        calls = [json.loads(line) for line in (self.root / 'argv.log').read_text().splitlines()]
        self.assertEqual([a[1] for a in calls], ['plan', 'run', 'show', 'output'])
        self.assertEqual(calls[0][calls[0].index('--authority') + 1], self.main)
        self.assertFalse(self.processes.children)

    def test_planning_refuses_a_bound_origin_that_claims_the_captured_main_commit(self):
        key, gen, task, _, _ = self.adapter()
        real_cas = task.cas

        def forged_origin(artifact_id):
            value = real_cas(artifact_id)
            if value.get('schema') == 'af.task-source-origin/3':
                value = dict(value, kind='bound')
            return value

        with mock.patch.object(task, 'cas', side_effect=forged_origin):
            with self.assertRaisesRegex(R.Refused, 'not current captured main'):
                task.execute(gen, self.journal, key, NOW)
        calls = [json.loads(line) for line in (self.root / 'argv.log').read_text().splitlines()]
        self.assertEqual([args[1] for args in calls], ['plan'])
        self.assertEqual(self.api.writes, [])

    def test_changed_cas_bytes_and_invalid_artifact_paths_are_refused(self):
        key, gen, task, _, evaluation_id = self.adapter()
        task.execute(gen, self.journal, key, NOW)
        hex_id = evaluation_id[7:]
        path = task.state / 'cas/objects' / hex_id[:2] / hex_id[2:]
        path.write_text('{}')
        with self.assertRaisesRegex(R.Refused, 'artifact bytes changed'):
            task.collect(gen, self.journal, key)
        with self.assertRaises(R.Refused):
            task.cas('sha256:../../secret')

    def test_missing_report_is_not_a_guessed_result(self):
        key, gen, task, config, _ = self.adapter()
        del config['show']['result']['outputs']['report']
        (self.root / 'cli.json').write_bytes(R.encoded(config))
        with self.assertRaises(R.TaskFailure):
            task.execute(gen, self.journal, key, NOW)
        self.assertEqual(self.api.writes, [])
        self.assertIsNotNone(self.journal.data['ledger'][key]['settled'])

    def rewrite_receipts(self, task, config, verification_id, change_evaluation=None, change_check=None):
        def put(value):
            return put_kernel(task, value)
        verification = task.cas(verification_id)
        evaluation = task.cas(verification['payload']['evaluation_id'])
        if change_check:
            check = task.cas(verification['payload']['check_receipt_id'])
            check['payload'].update(change_check)
            check_id = put(check)
            verification['payload']['check_receipt_id'] = check_id
            evaluation['payload']['check_receipt_id'] = check_id
        if change_evaluation:
            evaluation['payload'].update(change_evaluation)
        verification['payload']['evaluation_id'] = put(evaluation)
        config['show']['result']['outputs']['verification']['artifact_ids'] = [put(verification)]
        (self.root / 'cli.json').write_bytes(R.encoded(config))

    def test_authentic_evaluation_of_another_source_is_refused(self):
        key, gen, task, config, _ = self.adapter()
        task.execute(gen, self.journal, key, NOW)
        verification_id = config['show']['result']['outputs']['verification']['artifact_ids'][0]
        self.rewrite_receipts(task, config, verification_id, change_evaluation=dict(source_snapshot_id='sha256:' + 'f' * 64))
        with self.assertRaisesRegex(R.Refused, 'source identity refused'):
            task.collect(gen, self.journal, key)
        self.assertEqual(self.api.writes, [])

    def test_authentic_failed_check_receipt_is_refused_without_guessed_result(self):
        key, gen, task, config, _ = self.adapter()
        task.execute(gen, self.journal, key, NOW)
        verification_id = config['show']['result']['outputs']['verification']['artifact_ids'][0]
        self.rewrite_receipts(task, config, verification_id, change_check=dict(outcome='failed'))
        with self.assertRaisesRegex(R.Refused, 'report check did not pass'):
            task.collect(gen, self.journal, key)
        self.assertEqual(self.api.writes, [])

    def test_security_author_holds_with_missing_verifier_and_unknown_usage(self):
        key, gen, task, config, _ = self.adapter()
        task.execute(gen, self.journal, key, NOW)
        output = config['show']['result']['outputs']['report']
        envelope = task.cas(output['artifact_ids'][0])
        author = HELPERS.doc()
        author.update({'class': 'security', 'body_sha256': R.text_hash(gen['input']['body'])})
        envelope['payload']['text'] = HELPERS.rendered(author)
        output['artifact_ids'] = [put_kernel(task, envelope)]
        del config['show']['result']['outputs']['verification']
        config['show']['unknown_usage_attempts'] = 1
        (self.root / 'cli.json').write_bytes(R.encoded(config))
        self.runner.tasks = task
        self.runner.run_generation(key, gen)
        self.assertEqual(self.journal.issue(432)['hold']['security_triagers'], ['author'])
        self.assertIsNone(self.journal.data['ledger'][key]['settled'])
        self.assertEqual(self.notifications, [432])
        self.assertEqual(self.api.writes, [])

    def test_security_verifier_holds_before_unknown_usage_or_failed_check_public_fallback(self):
        key, gen, task, config, _ = self.adapter()
        task.execute(gen, self.journal, key, NOW)
        verifier = HELPERS.doc(**{'class': 'security', 'body_sha256': R.text_hash(gen['input']['body'])})
        verification_id = config['show']['result']['outputs']['verification']['artifact_ids'][0]
        self.rewrite_receipts(task, config, verification_id,
                              change_evaluation=dict(summary=HELPERS.evaluation(block=verifier)['summary']),
                              change_check=dict(outcome='failed'))
        config['show']['unknown_usage_attempts'] = 1
        del config['show']['result']['outputs']['report']
        (self.root / 'cli.json').write_bytes(R.encoded(config))
        self.runner.tasks = task
        self.runner.run_generation(key, gen)
        self.assertEqual(self.journal.issue(432)['hold']['security_triagers'], ['verifier'])
        self.assertIsNone(self.journal.data['ledger'][key]['settled'])
        self.assertEqual(self.api.writes, [])
        self.assertEqual(self.notifications, [432])

    def test_production_label_adapter_only_changes_authorized_differences(self):
        client = R.GitHub('owner/repo', 'fake-not-a-credential', self.lease, time.monotonic() + 5400, 'runner')
        calls = []
        client.call = lambda *args, **kwargs: calls.append((args, kwargs))
        client.set_labels(432, ['outside', 'class/bug'], ['outside', 'bug', 'triage/pending'])
        self.assertEqual([c[0][0] for c in calls], ['DELETE', 'DELETE', 'POST'])
        self.assertIn('triage%2Fpending', calls[1][0][1])
        with self.assertRaises(R.Refused):
            client.set_labels(432, [], ['outside'])
        with self.assertRaises(R.Refused):
            client.set_labels(432, [], ['class/decision'])

    def test_production_api_errors_are_sanitized_and_writes_not_retried(self):
        client = R.GitHub('owner/repo', 'fake-not-a-credential', self.lease, time.monotonic() + 5400, 'runner')
        with mock.patch.object(client.rest, 'call', side_effect=[(200, dict(login='runner')),
                                                               R.ApiRefused('secret server text')]) as call:
            with self.assertRaisesRegex(R.Ambiguous, 'outcome unknown') as raised:
                client.comment(432, 'body')
            self.assertNotIn('secret', str(raised.exception))
            self.assertEqual(call.call_count, 2)
            self.assertEqual([c.args[0] for c in call.call_args_list], ['GET', 'POST'])
        with self.assertRaises(R.Refused):
            R.GitHub('https://attacker.invalid', 'fake', self.lease, 0, 'runner')


class AdditionalRecovery(Fixture):
    def test_body_revert_is_a_new_event_and_unseen_nonpending_issue_is_not_backfill(self):
        row = issue(labels=['outside'])
        R.discover(self.journal, [row], 'runner', 'owner')
        self.assertEqual(self.journal.data['events'], {})
        for body, stamp in (('edited', '02:00:00'), ('See src/code.rs:3', '03:00:00'),
                            ('edited', '04:00:00')):
            row.update(body=body, updated_at='2026-11-02T' + stamp + 'Z')
            R.discover(self.journal, [row], 'runner', 'owner')
        self.assertEqual(len(self.journal.data['events']), 3)

    def test_new_event_invalidates_old_comment_before_planning_next_generation(self):
        self.queue()
        comment = copy.deepcopy(self.api.rows[432]['comments'][0])
        self.api.rows[432]['body'] += ' changed'
        R.discover(self.journal, [self.api.issue(432)], 'runner', 'owner')
        with self.assertRaises(R.Refused):
            R.consume_comment(comment, 432, self.journal)

    def test_superseded_dry_run_stays_fenced_and_new_event_can_run(self):
        self.runner.apply = False
        key = self.queue()
        gen = self.journal.data['generations'][key]
        self.api.rows[432]['body'] += ' changed'
        self.api.rows[432]['updated_at'] = '2026-11-02T02:00:00Z'
        R.discover(self.journal, [self.api.issue(432)], 'runner', 'owner')
        self.runner.apply = True
        with self.assertRaises(R.Refused):
            self.runner.run_generation(key, gen)
        self.assertEqual(gen['status'], 'dry-run')
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        self.runner.tick(time.monotonic() + 5400)
        self.assertEqual(gen['status'], 'stale')
        self.assertEqual(len(self.api.rows[432]['comments']), 1)
        self.assertEqual(self.tasks.calls, 2)

    def test_source_assembly_refusal_is_recorded_before_provider(self):
        with mock.patch.object(R, 'assemble', side_effect=R.Refused('source file bound refused')):
            self.queue()
        self.assertEqual(self.tasks.calls, 0)
        self.assertEqual(self.api.writes, [])
        self.assertTrue(self.journal.data['owner_review'])
        self.assertFalse(self.journal.data['ledger'])

    def test_over_reservation_usage_blocks_subsequent_admission_persistently(self):
        bundle = self.bundle()
        key, _ = self.journal.claim(R.generation_identity(432, bundle, self.main, 'one'), bundle, issue())
        self.journal.reserve(key, NOW)
        with self.assertRaises(R.Refused):
            self.journal.settle(key, dict(chargeable_tokens='150001', phase=dict(kind='finished')))
        next_key, _ = self.journal.claim(R.generation_identity(432, bundle, self.main, 'two'), bundle, issue())
        with self.assertRaisesRegex(R.Refused, 'ledger blocked'):
            self.journal.reserve(next_key, NOW)
        self.assertIsNotNone(R.strict_json(self.journal.path.read_bytes())['blocked'])

    def test_known_security_precedes_late_owner_conflict_and_stale_input(self):
        self.tasks.security = 'verifier'
        def changed():
            self.api.rows[432]['body'] += ' stale'
            self.api.rows[432]['labels'] += ['size/L', 'size/M']
            self.api.rows[432]['events'] += [dict(id=n, actor='owner', event='labeled', label=label,
                                                   created_at=NOW.isoformat())
                                             for n, label in ((5, 'size/L'), (6, 'size/M'))]
        self.tasks.change = changed
        self.queue()
        self.assertEqual(self.notifications, [432])
        self.assertEqual(self.api.writes, [])
        self.assertEqual(self.journal.issue(432)['hold']['security_triagers'], ['verifier'])

    def test_held_principal_cannot_consume_an_earlier_public_comment(self):
        self.queue()
        comment = copy.deepcopy(self.api.rows[432]['comments'][0])
        self.api.rows[432]['labels'].append('class/security')
        self.runner.held(self.api.issue(432))
        with self.assertRaises(R.Refused):
            R.consume_comment(comment, 432, self.journal)

    def test_sweep_replaces_pending_and_does_not_borrow_another_issues_fix(self):
        self.runner.sweep()
        self.assertEqual([l for l in self.api.rows[432]['labels'] if l.startswith('triage/')], ['triage/maybe-fixed'])
        for text in ('Fixed #999; see #432.', 'Fixed unrelated code. Related: #432.',
                     '#432 is related; completed #999.'):
            positive, review = R.sweep_evidence([dict(path='CHANGELOG.md', line=1, start=1, text=text)], [], 432)
            self.assertEqual(positive, [])
            self.assertEqual(len(review), 1)

    def test_forced_stop_and_denied_group_stop_keep_exclusion(self):
        proc = mock.Mock(pid=99999997)
        proc.poll.return_value = None
        with mock.patch.object(R.os, 'killpg', side_effect=ProcessLookupError):
            self.processes.stop(proc)
        self.assertTrue(self.lease.uncertain)
        self.lease.uncertain = False
        with mock.patch.object(R.os, 'killpg', side_effect=PermissionError):
            with self.assertRaisesRegex(R.Refused, 'stop denied'):
                self.processes.stop(proc)
        self.assertTrue(self.lease.uncertain)
        self.lease.uncertain = False


class FinalGuards(Fixture):
    def test_machine_lease_uncertain_flag_blocks_even_if_leaf_check_succeeds(self):
        lease = mock.Mock(uncertain=True)
        processes = R.Processes(lease, time.monotonic() + 5400)
        with mock.patch.object(R.subprocess, 'Popen') as spawn:
            with self.assertRaisesRegex(R.Refused, 'uncertain'):
                processes.run(['never-start'])
            spawn.assert_not_called()

    def test_anchor_disappearance_between_required_read_and_acquisition_defers(self):
        lease = mock.Mock(expected_identity=None, uncertain=False)
        with mock.patch.object(R.host_lock, 'read_anchor', return_value=[1, 2]), \
                mock.patch.object(R.host_lock, 'MachineLock', return_value=lease):
            with self.assertRaisesRegex(R.Refused, 'anchor changed'):
                R.anchored_lease()
        lease.release.assert_called_once()
        lease.guard.assert_not_called()

    def test_late_unrelated_label_invalidates_generation_and_preserves_the_label(self):
        original = self.api.set_labels
        def late_label(number, desired, current):
            original(number, desired, current)
            self.api.rows[number]['labels'].append('late-unrelated')
        self.api.set_labels = late_label
        with self.assertRaises(R.Refused):
            self.queue()
        key, gen = self.gen()
        self.assertEqual(gen['status'], 'stale')
        self.assertTrue(gen['labels_intent'])
        self.assertFalse(gen['labels_done'])
        self.assertFalse(gen.get('labels_aborted', False))
        self.assertEqual(self.api.rows[432]['comments'], [])
        self.assertTrue(any(k.startswith('stale-labels:') for k in self.journal.data['events']))
        self.api.set_labels = original
        self.runner.tick(time.monotonic() + 5400)
        self.assertIn('late-unrelated', self.api.rows[432]['labels'])
        self.assertEqual(len(self.api.rows[432]['comments']), 1)
        self.assertTrue(gen['labels_done'])
        self.assertFalse(gen.get('labels_aborted', False))

    def test_terminal_settlement_crossing_midnight_remains_charged_on_both_days(self):
        bundle = self.bundle()
        keys = []
        for n in range(14):
            key, _ = self.journal.claim(R.generation_identity(432, bundle, self.main, 'day%d' % n), bundle, issue())
            keys.append(key)
        for key in keys[:13]:
            self.journal.reserve(key, NOW)
        self.clock = NOW + dt.timedelta(days=1)
        for key in keys[:13]:
            self.journal.settle(key, dict(chargeable_tokens='150000', phase=dict(kind='finished')))
        with self.assertRaisesRegex(R.Refused, 'daily token cap'):
            self.journal.reserve(keys[13], self.clock)
        self.assertEqual(self.journal.data['ledger'][keys[0]]['settled_day'], '2026-11-03')

    def test_credential_permissions_and_verbatim_rubric_bound_refuse_before_calls(self):
        path = self.root / 'fake-credential'
        path.write_text('not-a-live-secret')
        path.chmod(0o644)
        with self.assertRaises(R.Refused):
            R.read_private(path, 4096, secret=True)
        path.chmod(0o600)
        self.assertEqual(R.read_private(path, 4096, secret=True), b'not-a-live-secret')
        with self.assertRaisesRegex(R.Refused, 'rubric exceeds'):
            R.bound_sources({'rubric': 'x' * (R.ENTRY_MAX + 1)})
        self.assertEqual(self.tasks.calls, 0)
        self.assertEqual(self.api.writes, [])

    def test_third_failure_preserves_owner_namespace_labels(self):
        self.tasks.failure = True
        self.api.rows[432]['labels'].append('size/L')
        self.api.rows[432]['events'].append(dict(id=1, actor='owner', event='labeled', label='size/L', created_at=NOW.isoformat()))
        for n in range(3):
            self.queue('failed-owner%d' % n)
        self.assertIn('size/L', self.api.rows[432]['labels'])
        self.assertIn('triage/needs-info', self.api.rows[432]['labels'])
        with self.assertRaises(R.Refused):
            R.consume_comment(self.api.rows[432]['comments'][0], 432, self.journal)


class ProvenanceAndPipes(Fixture):
    def test_unknown_label_provenance_suspends_before_task_or_public_effect(self):
        self.api.rows[432]['labels'].append('size/L')
        self.runner.tick(time.monotonic() + 5400)
        self.assertEqual(self.tasks.calls, 0)
        self.assertEqual(self.api.writes, [])
        self.assertIn('provenance', self.journal.issue(432)['suspension'])
        self.assertTrue(all(e['status'] == 'suspended' for e in self.journal.data['events'].values()))

    def test_owned_stdin_is_drained_under_the_same_process_wall(self):
        fake = self.root / 'fake-af'
        fake.write_text('#!%s\nimport sys\nprint(len(sys.stdin.buffer.read()))\n' % sys.executable)
        fake.chmod(0o700)
        self.assertEqual(self.processes.run([str(fake)], data=b'x' * 100000), b'100000\n')
        self.assertFalse(self.processes.children)


class KernelCasIdentity(Fixture):
    def test_cas_reader_accepts_pinned_kernel_schema_envelope_not_raw_sha256(self):
        fixture = json.loads((ROOT / 'fixtures/performance/schema/kernel-receipts/valid-fixture.json').read_bytes())
        envelope = fixture['tasks'][0]['outputs']['baseline']['envelope']
        identity = 'sha256:a6d617c083b6afef25fcbc7491ec37102f7de11dda6f97eb81f631ac10b5c05e'
        self.assertEqual(envelope['artifact_id'], identity)
        task = R.Task(self.root, self.repo, self.processes)
        path = task.state / 'cas/objects' / identity[7:9] / identity[9:]
        path.parent.mkdir(parents=True)
        path.write_bytes(kernel_bytes(envelope))  # Retain the fixture's IDs; do not regenerate them.
        self.assertEqual(task.cas(identity), envelope)
        alias = R.kernel_contracts.kernel_blob_id(path.read_bytes())
        alias_path = task.state / 'cas/objects' / alias[7:9] / alias[9:]
        alias_path.parent.mkdir(parents=True, exist_ok=True)
        alias_path.write_bytes(path.read_bytes())
        with self.assertRaises(R.Refused):
            task.cas(alias)  # A content alias cannot stand in for an artifact identity.
        self.assertNotEqual(identity, 'sha256:' + R.text_hash(path.read_text()))
        envelope['producer']['run_id'] = 'tampered'
        path.write_bytes(kernel_bytes(envelope))
        with self.assertRaisesRegex(R.Refused, 'artifact bytes changed'):
            task.cas(identity)

    def test_content_domain_matches_fixed_rust_canonical_byte_vector(self):
        import hashlib
        raw = b'{"a":1}'
        identity = 'sha256:' + hashlib.sha256(b'review.kernel/content-id/v1\0' + raw).hexdigest()
        task = R.Task(self.root, self.repo, self.processes)
        path = task.state / 'cas/objects' / identity[7:9] / identity[9:]
        path.parent.mkdir(parents=True)
        path.write_bytes(raw)
        self.assertEqual(task.cas(identity), {'a': 1})


class HoldReleaseFence(Fixture):
    def test_owner_removal_before_security_hold_record_does_not_release_that_hold(self):
        self.tasks.security = 'author'
        def earlier_removal():
            self.api.rows[432]['events'].append(dict(id=10, actor='owner', event='unlabeled',
                label='triage/held', created_at=(NOW - dt.timedelta(seconds=1)).isoformat()))
        self.tasks.change = earlier_removal
        self.queue()
        self.tasks.security, self.tasks.change = None, None
        self.queue('still-held')
        self.assertEqual(self.tasks.calls, 1)
        self.assertIsNotNone(self.journal.issue(432)['hold'])
        self.assertEqual(self.api.writes, [])


class IdentityFailureFence(Fixture):
    def test_known_spend_never_turns_uncertain_identity_or_input_reads_into_public_needs_info(self):
        def uncertain(gen, journal, key):
            journal.settle(key, dict(chargeable_tokens='1000', phase=dict(kind='finished')))
            raise R.Refused('report/evaluation source identity refused')
        self.tasks.collect = uncertain
        for n in range(3):
            self.queue('identity-%d' % n)
        self.assertEqual(self.journal.issue(432)['failures'], 0)
        self.assertEqual(self.api.writes, [])
        self.assertTrue(all(g['status'] == 'uncertain' for g in self.journal.data['generations'].values()))


class SweepUncertaintyFence(Fixture):
    def test_tick_during_owner_hold_preserves_dry_run_until_checked_replacement_publishes(self):
        self.runner.apply = False
        self.runner.tick(time.monotonic() + 5400)
        key = self.journal.issue(432)['current_generation']
        self.assertEqual(self.journal.data['generations'][key]['status'], 'dry-run')
        self.runner.apply = True
        row = self.api.rows[432]
        row['labels'].append('triage/held')
        row['events'].append(dict(id=1, actor='owner', event='labeled', label='triage/held',
                                  created_at=self.clock.isoformat()))
        self.runner.tick(time.monotonic() + 5400)
        self.runner.sweep()
        saved = R.strict_json(self.journal.path.read_bytes())
        self.assertEqual(saved['generations'][key]['status'], 'dry-run')
        self.assertIsNone(saved['issues']['432']['current_generation'])
        self.assertIsNotNone(saved['issues']['432']['hold'])
        self.assertEqual(self.notifications, [432])
        self.assertEqual(self.api.writes, [])
        self.assertEqual(self.tasks.calls, 1)

        # Recover the journal and release the hold without another triage tick.
        self.journal.data = saved
        self.clock += dt.timedelta(minutes=1)
        row['labels'].remove('triage/held')
        row['events'].append(dict(id=2, actor='owner', event='unlabeled', label='triage/held',
                                  created_at=self.clock.isoformat()))
        self.runner.sweep()
        saved = R.strict_json(self.journal.path.read_bytes())
        self.assertIsNone(saved['issues']['432']['hold'])
        self.assertEqual(saved['generations'][key]['status'], 'dry-run')
        self.assertTrue(self.runner.effects_blocked(432))
        self.assertEqual(self.api.writes, [])

        publish = self.runner.publish
        def checked_publish(replacement, gen):
            durable = R.strict_json(self.journal.path.read_bytes())['generations']
            self.assertNotEqual(replacement, key)
            self.assertEqual(durable[replacement]['status'], 'checked')
            self.assertEqual(durable[key]['status'], 'dry-run')
            self.runner.sweep()
            self.assertEqual(self.api.writes, [])
            return publish(replacement, gen)
        set_labels = self.api.set_labels
        def checked_write(number, desired, current):
            durable = R.strict_json(self.journal.path.read_bytes())
            replacement = durable['issues']['432']['current_generation']
            self.assertEqual(durable['generations'][key]['status'], 'dry-run')
            self.assertEqual(durable['generations'][replacement]['status'], 'publishing')
            self.runner.sweep()
            self.assertEqual(self.api.writes, [])
            return set_labels(number, desired, current)
        with mock.patch.object(self.runner, 'publish', side_effect=checked_publish), \
                mock.patch.object(self.api, 'set_labels', side_effect=checked_write):
            self.runner.tick(time.monotonic() + 5400)
        saved = R.strict_json(self.journal.path.read_bytes())
        replacement = saved['issues']['432']['current_generation']
        self.assertNotEqual(replacement, key)
        self.assertEqual(saved['generations'][key]['status'], 'stale')
        self.assertEqual(saved['generations'][replacement]['status'], 'published')
        self.assertEqual(R.consume_comment(row['comments'][0], 432, self.journal),
                         saved['generations'][replacement]['record'])
        self.assertEqual([w[0] for w in self.api.writes], ['labels', 'comment'])
        self.assertEqual(self.tasks.calls, 2)
        self.runner.sweep()
        self.assertIn('triage/maybe-fixed', row['labels'])
        self.assertEqual(len(row['comments']), 2)
        self.assertIn('triage-sweep evidence=', row['comments'][1]['body'])
        self.assertEqual(self.tasks.calls, 2)

    def test_dry_run_retirement_requires_a_checked_replacement_not_a_pointer_or_claim(self):
        self.runner.apply = False
        key = self.queue()
        old = self.journal.data['generations'][key]
        state = self.journal.issue(432)
        for pointer in (None, '0' * 64):
            with self.subTest(pointer=pointer):
                state['current_generation'] = pointer
                self.journal.save()
                self.runner.retire_superseded_dry_runs()
                saved = R.strict_json(self.journal.path.read_bytes())['generations'][key]
                self.assertEqual(saved['status'], 'dry-run')
                self.assertTrue(self.runner.effects_blocked(432))

        identity = dict(old['identity'], event_id='replacement')
        replacement, gen = self.journal.claim(identity, old['bundle'], old['input'])
        self.assertNotEqual(replacement, key)
        for status in ('claimed', 'planning', 'admitting', 'uncertain', 'failed', 'held', 'suspended', 'stale',
                       'checked', 'dry-run', 'publishing', 'published'):
            with self.subTest(status=status):
                gen['status'] = status
                self.journal.save()
                self.runner.retire_superseded_dry_runs()
                saved = R.strict_json(self.journal.path.read_bytes())['generations'][key]
                self.assertEqual(saved['status'], 'dry-run')
                self.assertTrue(self.runner.effects_blocked(432))
        self.assertEqual(self.api.writes, [])

    def test_dry_run_blocks_sweep_after_apply_until_checked_proposal_is_published(self):
        self.runner.apply = False
        key = self.queue()
        gen = self.journal.data['generations'][key]
        saved = R.strict_json(self.journal.path.read_bytes())['generations'][key]
        self.assertEqual(saved['status'], 'dry-run')
        self.assertEqual(saved['desired_labels'], gen['desired_labels'])
        self.assertEqual(saved['comment'], gen['comment'])
        self.assertTrue(R.sweep_evidence(self.manifest.citations(432), [], 432)[0])
        self.runner.apply = True
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        self.assertEqual(self.journal.issue(432)['current_generation'], key)
        self.assertEqual(gen['status'], 'dry-run')
        self.runner.run_generation(key, gen)
        self.assertEqual(gen['status'], 'published')
        self.assertEqual(R.consume_comment(self.api.rows[432]['comments'][0], 432, self.journal), gen['record'])
        self.runner.sweep()
        self.assertIn('triage/maybe-fixed', self.api.rows[432]['labels'])
        self.assertEqual(len(self.api.rows[432]['comments']), 2)
        self.assertIn('triage-sweep evidence=', self.api.rows[432]['comments'][1]['body'])
        self.assertEqual(self.tasks.calls, 1)

    def test_dry_run_owner_hold_then_release_keeps_sweep_fenced_until_resolution(self):
        self.runner.apply = False
        key = self.queue()
        self.runner.apply = True
        row = self.api.rows[432]
        row['labels'].append('triage/held')
        row['events'].append(dict(id=1, actor='owner', event='labeled', label='triage/held',
                                  created_at=self.clock.isoformat()))
        self.runner.run_generation(key, self.journal.data['generations'][key])
        self.runner.sweep()
        saved = R.strict_json(self.journal.path.read_bytes())
        self.assertEqual(saved['generations'][key]['status'], 'dry-run')
        self.assertIsNotNone(saved['issues']['432']['hold'])
        self.assertEqual(self.notifications, [432])
        self.assertEqual(self.api.writes, [])
        self.assertEqual(len(self.output), 1)  # Only the original dry-run proposal, no public hold.
        self.assertEqual(self.tasks.calls, 1)

        # Recover from the durable journal, then observe a later owner release.
        self.journal.data = saved
        self.clock += dt.timedelta(minutes=1)
        row['labels'].remove('triage/held')
        row['events'].append(dict(id=2, actor='owner', event='unlabeled', label='triage/held',
                                  created_at=self.clock.isoformat()))
        self.runner.sweep()
        self.assertIsNone(self.journal.issue(432)['hold'])
        self.assertEqual(self.api.writes, [])
        self.assertTrue(self.runner.effects_blocked(432))
        # Discovery queues the owner events; ordinary recovery retires the proposal
        # only with the fresh checked replacement's successful publication.
        self.runner.tick(time.monotonic() + 5400)
        self.assertEqual(R.strict_json(self.journal.path.read_bytes())['generations'][key]['status'], 'stale')
        replacement = self.journal.issue(432)['current_generation']
        self.assertNotEqual(replacement, key)
        self.assertEqual(self.journal.data['generations'][replacement]['status'], 'published')
        self.runner.sweep()
        self.assertIn('triage/maybe-fixed', row['labels'])
        self.assertEqual(len(row['comments']), 2)

    def test_tick_retires_superseded_dry_run_with_published_replacement_before_sweep(self):
        self.runner.apply = False
        old_key = self.queue()
        old = self.journal.data['generations'][old_key]
        self.runner.apply = True
        # Stop after the replacement is durably checked, before publication starts.
        with mock.patch.object(self.runner, 'publish', side_effect=R.Refused('publication interrupted')):
            with self.assertRaisesRegex(R.Refused, 'publication interrupted'):
                self.queue('replacement')
        key = self.journal.data['events']['replacement']['generation']
        gen = self.journal.data['generations'][key]
        self.assertNotEqual(key, old_key)
        self.assertEqual(gen['status'], 'checked')
        self.assertEqual(self.api.writes, [])
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])

        set_labels = self.api.set_labels
        def checked_write(number, desired, current):
            saved = R.strict_json(self.journal.path.read_bytes())['generations']
            self.assertEqual(saved[old_key]['status'], 'dry-run')
            self.assertEqual(saved[key]['status'], 'publishing')
            self.runner.sweep()  # Replacement still unresolved; no auxiliary effect may interleave.
            self.assertEqual(self.api.writes, [])
            return set_labels(number, desired, current)
        with mock.patch.object(self.api, 'set_labels', side_effect=checked_write):
            self.runner.tick(time.monotonic() + 5400)
        self.assertEqual(old['status'], 'stale')
        self.assertEqual(gen['status'], 'published')
        saved = R.strict_json(self.journal.path.read_bytes())['generations']
        self.assertEqual(saved[old_key]['status'], 'stale')
        self.assertEqual(saved[key]['status'], 'published')
        self.assertFalse(old['labels_done'])
        self.assertFalse(old['comment_done'])
        self.assertEqual(R.consume_comment(self.api.rows[432]['comments'][0], 432, self.journal), gen['record'])
        self.runner.sweep()
        self.assertIn('triage/maybe-fixed', self.api.rows[432]['labels'])
        self.assertEqual(len(self.api.rows[432]['comments']), 2)
        self.assertEqual(self.tasks.calls, 2)

    def test_replacement_first_publication_retires_the_old_dry_run(self):
        self.runner.apply = False
        old_key = self.queue()
        self.runner.apply = True
        key = self.queue('replacement')
        saved = R.strict_json(self.journal.path.read_bytes())['generations']
        self.assertEqual(saved[old_key]['status'], 'stale')
        self.assertEqual(saved[key]['status'], 'published')
        self.assertEqual(len(self.api.rows[432]['comments']), 1)
        self.assertEqual(self.tasks.calls, 2)

    def test_superseded_dry_run_with_label_or_comment_intent_remains_fenced(self):
        self.runner.apply = False
        old_key = self.queue()
        old = self.journal.data['generations'][old_key]
        self.runner.apply = True
        with mock.patch.object(self.runner, 'publish', side_effect=R.Refused('publication interrupted')):
            with self.assertRaises(R.Refused):
                self.queue('replacement')
        key = self.journal.data['events']['replacement']['generation']
        for phase in ('labels', 'comment'):
            with self.subTest(phase=phase):
                old['labels_intent'] = phase == 'labels'
                old['comment_intent'] = phase == 'comment'
                self.journal.save()
                with self.assertRaisesRegex(R.Refused, 'earlier issue generation has unresolved effects'):
                    self.runner.tick(time.monotonic() + 5400)
                self.assertEqual(old['status'], 'dry-run')
                self.assertEqual(self.journal.data['generations'][key]['status'], 'checked')
                saved = R.strict_json(self.journal.path.read_bytes())['generations'][old_key]
                self.assertTrue(saved[phase + '_intent'])
                self.assertFalse(saved[phase + '_done'])
                self.runner.sweep()
                self.assertEqual(self.api.writes, [])

    def test_freshness_invalidation_fences_dry_run_until_tick_publishes_replacement(self):
        self.runner.apply = False
        key = self.queue()
        gen = self.journal.data['generations'][key]
        self.runner.apply = True
        self.api.rows[432]['body'] += ' new evidence'
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        self.assertEqual(gen['status'], 'dry-run')
        with self.assertRaisesRegex(R.Refused, 'stale inputs; new generation queued'):
            self.runner.run_generation(key, gen)
        self.assertEqual(gen['status'], 'dry-run')
        self.assertTrue(any(k.startswith('stale:') for k in self.journal.data['events']))
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        self.assertEqual(self.tasks.calls, 1)
        self.journal.data = R.strict_json(self.journal.path.read_bytes())
        self.runner.tick(time.monotonic() + 5400)
        saved = R.strict_json(self.journal.path.read_bytes())
        replacement = saved['issues']['432']['current_generation']
        self.assertNotEqual(replacement, key)
        self.assertEqual(saved['generations'][key]['status'], 'stale')
        self.assertEqual(saved['generations'][replacement]['status'], 'published')
        self.assertEqual([w[0] for w in self.api.writes], ['labels', 'comment'])
        self.assertEqual(R.consume_comment(self.api.rows[432]['comments'][0], 432, self.journal),
                         saved['generations'][replacement]['record'])
        self.runner.sweep()
        self.assertIn('triage/maybe-fixed', self.api.rows[432]['labels'])
        self.assertEqual(len(self.api.rows[432]['comments']), 2)
        self.assertIn('triage-sweep evidence=', self.api.rows[432]['comments'][1]['body'])
        self.assertEqual(self.tasks.calls, 2)

    def test_tick_replacement_stale_at_publication_keeps_original_until_later_fresh_replacement(self):
        self.runner.apply = False
        self.runner.tick(time.monotonic() + 5400)
        old_key = self.journal.issue(432)['current_generation']
        self.runner.apply = True
        self.api.rows[432]['body'] += ' first edit'
        publish = self.runner.publish
        def changed_after_check(key, gen):
            saved = R.strict_json(self.journal.path.read_bytes())['generations']
            self.assertEqual(saved[key]['status'], 'checked')
            self.assertEqual(saved[old_key]['status'], 'dry-run')
            self.api.rows[432]['body'] += ' edit after discovery and check'
            return publish(key, gen)
        with mock.patch.object(self.runner, 'publish', side_effect=changed_after_check):
            with self.assertRaisesRegex(R.Refused, 'stale inputs; new generation queued'):
                self.runner.tick(time.monotonic() + 5400)
        saved = R.strict_json(self.journal.path.read_bytes())
        rejected = saved['issues']['432']['current_generation']
        self.assertNotEqual(rejected, old_key)
        self.assertEqual(saved['generations'][rejected]['status'], 'stale')
        self.assertEqual(saved['generations'][old_key]['status'], 'dry-run')
        self.assertTrue(any(k.startswith('stale:') and v['status'] == 'queued'
                            for k, v in saved['events'].items()))
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        self.assertEqual(self.tasks.calls, 2)
        self.journal.data = saved
        self.runner.tick(time.monotonic() + 5400)
        saved = R.strict_json(self.journal.path.read_bytes())
        replacement = saved['issues']['432']['current_generation']
        self.assertNotIn(replacement, (old_key, rejected))
        self.assertEqual(saved['generations'][old_key]['status'], 'stale')
        self.assertEqual(saved['generations'][rejected]['status'], 'stale')
        self.assertEqual(saved['generations'][replacement]['status'], 'published')
        self.assertEqual(R.consume_comment(self.api.rows[432]['comments'][0], 432, self.journal),
                         saved['generations'][replacement]['record'])
        self.assertEqual([w[0] for w in self.api.writes], ['labels', 'comment'])
        self.runner.sweep()
        self.assertIn('triage/maybe-fixed', self.api.rows[432]['labels'])
        self.assertEqual(len(self.api.rows[432]['comments']), 2)
        self.assertEqual(self.tasks.calls, 3)

    def test_tick_replacement_refused_before_write_keeps_original_fence(self):
        self.runner.apply = False
        self.runner.tick(time.monotonic() + 5400)
        old_key = self.journal.issue(432)['current_generation']
        self.runner.apply = True
        self.api.rows[432]['body'] += ' first edit'
        set_labels = self.api.set_labels
        def changed_before_write(*args):
            self.api.rows[432]['body'] += ' late edit'
            return set_labels(*args)  # The ordinary API freshness guard refuses this request.
        with mock.patch.object(self.api, 'set_labels', side_effect=changed_before_write):
            with self.assertRaisesRegex(R.Refused, 'stale inputs; new generation queued'):
                self.runner.tick(time.monotonic() + 5400)
        saved = R.strict_json(self.journal.path.read_bytes())
        rejected = saved['issues']['432']['current_generation']
        self.assertEqual(saved['generations'][rejected]['status'], 'stale')
        self.assertTrue(saved['generations'][rejected]['labels_aborted'])
        self.assertFalse(saved['generations'][rejected]['labels_intent'])
        self.assertEqual(saved['generations'][old_key]['status'], 'dry-run')
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        self.journal.data = saved
        self.runner.tick(time.monotonic() + 5400)
        saved = R.strict_json(self.journal.path.read_bytes())
        replacement = saved['issues']['432']['current_generation']
        self.assertNotIn(replacement, (old_key, rejected))
        self.assertEqual(saved['generations'][old_key]['status'], 'stale')
        self.assertEqual(saved['generations'][replacement]['status'], 'published')
        self.assertEqual(self.tasks.calls, 3)
        self.runner.sweep()
        self.assertIn('triage/maybe-fixed', self.api.rows[432]['labels'])

    def test_label_freshness_invalidation_keeps_dry_run_fenced_until_replacement(self):
        self.runner.apply = False
        self.runner.tick(time.monotonic() + 5400)
        key = self.journal.issue(432)['current_generation']
        self.runner.apply = True
        self.api.rows[432]['labels'].append('outside-new')
        with self.assertRaisesRegex(R.Refused, 'labels changed outside publication intent'):
            self.runner.tick(time.monotonic() + 5400)
        saved = R.strict_json(self.journal.path.read_bytes())
        self.assertEqual(saved['generations'][key]['status'], 'dry-run')
        self.assertIsNone(saved['issues']['432']['current_generation'])
        self.assertTrue(any(k.startswith('stale-labels:') for k in saved['events']))
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        self.journal.data = saved
        self.runner.tick(time.monotonic() + 5400)
        replacement = self.journal.issue(432)['current_generation']
        self.assertNotEqual(replacement, key)
        self.assertEqual(self.journal.data['generations'][key]['status'], 'stale')
        self.assertEqual(self.journal.data['generations'][replacement]['status'], 'published')
        self.assertIn('outside-new', self.api.rows[432]['labels'])
        self.runner.sweep()
        self.assertIn('triage/maybe-fixed', self.api.rows[432]['labels'])
        self.assertEqual(self.tasks.calls, 2)

    def test_dry_run_own_publication_refused_before_write_retains_proposal(self):
        self.runner.apply = False
        self.runner.tick(time.monotonic() + 5400)
        key = self.journal.issue(432)['current_generation']
        self.runner.apply = True
        set_labels = self.api.set_labels
        def changed_before_write(*args):
            self.api.rows[432]['body'] += ' late edit'
            return set_labels(*args)
        with mock.patch.object(self.api, 'set_labels', side_effect=changed_before_write):
            with self.assertRaisesRegex(R.Refused, 'stale inputs; new generation queued'):
                self.runner.tick(time.monotonic() + 5400)
        saved = R.strict_json(self.journal.path.read_bytes())
        self.assertEqual(saved['generations'][key]['status'], 'dry-run')
        self.assertTrue(saved['generations'][key]['labels_aborted'])
        self.assertFalse(saved['generations'][key]['labels_intent'])
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        self.journal.data = saved
        self.runner.tick(time.monotonic() + 5400)
        self.assertEqual(self.journal.data['generations'][key]['status'], 'stale')
        self.assertEqual(self.tasks.calls, 2)
        self.runner.sweep()
        self.assertIn('triage/maybe-fixed', self.api.rows[432]['labels'])

    def test_tick_completed_dry_run_labels_invalidated_before_comment_allow_checked_replacement(self):
        self.assert_completed_dry_run_effect_allows_replacement('labels')

    def test_tick_labels_invalidated_after_write_before_completion_reconcile_before_replacement(self):
        self.assert_completed_dry_run_effect_allows_replacement('labels', before_completion=True)

    def test_tick_completed_dry_run_comment_invalidated_before_handoff_allows_checked_replacement(self):
        self.assert_completed_dry_run_effect_allows_replacement('comment')

    def assert_completed_dry_run_effect_allows_replacement(self, phase, before_completion=False):
        self.runner.apply = False
        self.runner.tick(time.monotonic() + 5400)
        old_key = self.journal.issue(432)['current_generation']
        self.runner.apply = True
        fresh = self.runner.fresh
        set_labels = self.api.set_labels
        def changed_after_write(*args):
            result = set_labels(*args)
            if before_completion:
                saved = R.strict_json(self.journal.path.read_bytes())['generations'][old_key]
                self.assertTrue(saved['labels_intent'])
                self.assertFalse(saved['labels_done'])
                self.api.rows[432]['body'] += ' evidence after labels returned'
            return result
        def changed_after_completion(gen):
            saved = R.strict_json(self.journal.path.read_bytes())['generations'][old_key]
            if saved[phase + '_done']:
                self.assertTrue(saved['labels_intent'])
                self.assertEqual(saved['comment_done'], phase == 'comment')
                self.assertEqual(saved.get('comment_intent', False), phase == 'comment')
                self.api.rows[432]['body'] += ' evidence after completed ' + phase
            return fresh(gen)
        with mock.patch.object(self.runner, 'fresh', side_effect=changed_after_completion), \
                mock.patch.object(self.api, 'set_labels', side_effect=changed_after_write):
            with self.assertRaisesRegex(R.Refused, 'stale inputs; new generation queued'):
                self.runner.tick(time.monotonic() + 5400)
        saved = R.strict_json(self.journal.path.read_bytes())
        old = saved['generations'][old_key]
        self.assertEqual(old['status'], 'dry-run')
        self.assertTrue(old['labels_intent'])
        self.assertEqual(old['labels_done'], not before_completion)
        self.assertFalse(old.get('labels_aborted', False))
        self.assertEqual(old['comment_done'], phase == 'comment')
        self.assertIsNone(saved['issues']['432']['current_generation'])
        self.assertTrue(any(k.startswith('stale:') and v['status'] == 'queued'
                            for k, v in saved['events'].items()))
        writes = copy.deepcopy(self.api.writes)
        self.assertEqual([w[0] for w in writes], ['labels'] + (['comment'] if phase == 'comment' else []))
        self.runner.sweep()
        self.assertEqual(self.api.writes, writes)
        self.assertEqual(len(self.api.rows[432]['comments']), int(phase == 'comment'))

        # Reload durable checkpoints and invalidate no further inputs. Ordinary recovery
        # must keep sweep fenced throughout the replacement's checked publication.
        self.journal.data = saved
        publish = self.runner.publish
        def checked_publish(key, gen):
            durable = R.strict_json(self.journal.path.read_bytes())['generations']
            self.assertNotEqual(key, old_key)
            self.assertEqual(durable[key]['status'], 'checked')
            self.assertEqual(durable[old_key]['status'], 'dry-run')
            self.assertTrue(durable[old_key]['labels_done'])
            self.assertFalse(durable[old_key].get('labels_aborted', False))
            self.runner.sweep()
            self.assertEqual(self.api.writes, writes)
            return publish(key, gen)
        comment = self.api.comment
        def checked_comment(*args):
            durable = R.strict_json(self.journal.path.read_bytes())['generations']
            self.assertEqual(durable[old_key]['status'], 'dry-run')
            before = copy.deepcopy(self.api.writes)
            self.runner.sweep()
            self.assertEqual(self.api.writes, before)
            return comment(*args)
        with mock.patch.object(self.runner, 'publish', side_effect=checked_publish), \
                mock.patch.object(self.api, 'comment', side_effect=checked_comment):
            self.runner.tick(time.monotonic() + 5400)
        saved = R.strict_json(self.journal.path.read_bytes())
        replacement = saved['issues']['432']['current_generation']
        self.assertNotEqual(replacement, old_key)
        self.assertEqual(saved['generations'][old_key]['status'], 'stale')
        self.assertTrue(saved['generations'][old_key]['labels_done'])
        self.assertEqual(saved['generations'][replacement]['status'], 'published')
        self.assertEqual(saved['issues']['432']['published'], replacement)
        self.assertEqual(R.consume_comment(self.api.rows[432]['comments'][0], 432, self.journal),
                         saved['generations'][replacement]['record'])
        self.assertEqual([w[0] for w in self.api.writes], [w[0] for w in writes] + ['labels', 'comment'])
        self.assertEqual(self.tasks.calls, 2)
        self.runner.sweep()
        self.assertIn('triage/maybe-fixed', self.api.rows[432]['labels'])
        self.assertEqual(len(self.api.rows[432]['comments']), 2)
        self.assertIn('triage-sweep evidence=', self.api.rows[432]['comments'][1]['body'])
        self.assertEqual(self.tasks.calls, 2)

    def test_tick_unknown_dry_run_label_attempt_blocks_replacement_and_sweep(self):
        self.runner.apply = False
        self.runner.tick(time.monotonic() + 5400)
        old_key = self.journal.issue(432)['current_generation']
        self.runner.apply = True
        def unknown_labels(number, desired, current):
            self.api.before_write()
            self.api.writes.append(('labels', number, desired))
            raise R.Ambiguous('lost label response')
        with mock.patch.object(self.api, 'set_labels', side_effect=unknown_labels):
            with self.assertRaisesRegex(R.Ambiguous, 'lost label response'):
                self.runner.tick(time.monotonic() + 5400)
        saved = R.strict_json(self.journal.path.read_bytes())
        self.assertTrue(saved['generations'][old_key]['labels_intent'])
        self.assertFalse(saved['generations'][old_key]['labels_done'])
        self.assertFalse(saved['generations'][old_key].get('labels_aborted', False))
        self.assertEqual([w[0] for w in self.api.writes], ['labels'])
        writes = copy.deepcopy(self.api.writes)
        self.api.rows[432]['body'] += ' new evidence after ambiguous labels'
        self.journal.data = saved
        with self.assertRaisesRegex(R.Refused, 'earlier issue generation has unresolved effects'):
            self.runner.tick(time.monotonic() + 5400)
        saved = R.strict_json(self.journal.path.read_bytes())
        replacement = saved['issues']['432']['current_generation']
        self.assertNotEqual(replacement, old_key)
        self.assertEqual(saved['generations'][replacement]['status'], 'checked')
        self.assertEqual(saved['generations'][old_key]['status'], 'dry-run')
        self.runner.retire_superseded_dry_runs()
        self.runner.sweep()
        self.assertEqual(self.api.writes, writes)
        self.assertEqual(self.api.rows[432]['comments'], [])
        self.assertEqual(R.strict_json(self.journal.path.read_bytes())['generations'][old_key]['status'], 'dry-run')

    def test_tick_partial_label_write_then_freshness_refusal_keeps_replacement_and_sweep_fenced(self):
        self.runner.apply = False
        self.runner.tick(time.monotonic() + 5400)
        old_key = self.journal.issue(432)['current_generation']
        self.runner.apply = True
        client = R.GitHub('owner/repo', 'fake', self.lease, time.monotonic() + 5400, 'runner')
        client.before_write = lambda: self.api.before_write()
        def transport(method, path, **kwargs):
            if method == 'GET' and path == '/user':
                return 200, dict(login='runner')
            self.assertEqual((method, path), ('DELETE', '/repos/owner/repo/issues/432/labels/bug'))
            row = self.api.rows[432]
            row['labels'].remove('bug')
            row['events'].append(dict(id=1, actor='runner', event='unlabeled', label='bug',
                                      created_at=NOW.isoformat()))
            self.api.writes.append(('labels', 432, list(row['labels'])))
            row['body'] += ' evidence after first DELETE'
            return 204, None
        with mock.patch.object(self.api, 'set_labels', side_effect=client.set_labels), \
                mock.patch.object(client.rest, 'call', side_effect=transport) as requests:
            with self.assertRaisesRegex(R.Ambiguous, 'partial label write'):
                self.runner.tick(time.monotonic() + 5400)
        self.assertEqual([call.args[0] for call in requests.call_args_list], ['GET', 'DELETE'])
        saved = R.strict_json(self.journal.path.read_bytes())
        old = saved['generations'][old_key]
        self.assertTrue(old['labels_intent'])
        self.assertFalse(old['labels_done'])
        self.assertFalse(old.get('labels_aborted', False))
        self.assertIsNone(saved['issues']['432']['current_generation'])
        self.assertNotEqual(self.api.rows[432]['labels'], old['desired_labels'])
        writes = copy.deepcopy(self.api.writes)
        self.journal.data = saved
        with self.assertRaisesRegex(R.Refused, 'earlier issue generation has unresolved effects'):
            self.runner.tick(time.monotonic() + 5400)
        saved = R.strict_json(self.journal.path.read_bytes())
        replacement = saved['issues']['432']['current_generation']
        self.assertNotEqual(replacement, old_key)
        self.assertEqual(saved['generations'][replacement]['status'], 'checked')
        self.assertEqual(saved['generations'][old_key]['status'], 'dry-run')
        self.assertTrue(saved['generations'][old_key]['labels_intent'])
        self.assertFalse(saved['generations'][old_key]['labels_done'])
        self.assertFalse(saved['generations'][old_key].get('labels_aborted', False))
        self.runner.retire_superseded_dry_runs()
        self.runner.sweep()
        self.assertEqual(self.api.writes, writes)
        self.assertEqual(self.api.rows[432]['comments'], [])
        self.assertEqual(R.strict_json(self.journal.path.read_bytes())['generations'][old_key]['status'], 'dry-run')

        # An observed completed result, rather than another refused/returned call, resolves
        # the original intent. The already checked replacement can then publish on a tick.
        with mock.patch.object(self.api, 'before_write', None):
            self.api.set_labels(432, old['desired_labels'], self.api.rows[432]['labels'])
        self.journal.data = saved
        self.runner.tick(time.monotonic() + 5400)
        saved = R.strict_json(self.journal.path.read_bytes())
        self.assertTrue(saved['generations'][old_key]['labels_done'])
        self.assertFalse(saved['generations'][old_key].get('labels_aborted', False))
        self.assertEqual(saved['generations'][old_key]['status'], 'stale')
        self.assertEqual(saved['generations'][replacement]['status'], 'published')
        self.assertEqual(saved['issues']['432']['published'], replacement)
        self.assertEqual(self.tasks.calls, 2)
        self.runner.sweep()
        self.assertIn('triage/maybe-fixed', self.api.rows[432]['labels'])

    def test_prewrite_refusal_cannot_abort_an_earlier_unknown_label_attempt(self):
        self.runner.apply = False
        key = self.queue()
        gen = self.journal.data['generations'][key]
        self.runner.apply = True
        with mock.patch.object(self.api, 'set_labels', side_effect=R.Refused('initial prewrite refusal')):
            with self.assertRaisesRegex(R.Refused, 'initial prewrite refusal'):
                self.runner.publish(key, gen)
        old = R.strict_json(self.journal.path.read_bytes())['generations'][key]
        self.assertTrue(old['labels_aborted'])
        self.assertFalse(old['labels_intent'])
        with mock.patch.object(self.api, 'set_labels', side_effect=R.Ambiguous('unknown labels')):
            with self.assertRaises(R.Ambiguous):
                self.runner.publish(key, gen)
        saved = R.strict_json(self.journal.path.read_bytes())
        self.assertFalse(saved['generations'][key].get('labels_aborted', False))
        self.journal.data = saved
        gen = saved['generations'][key]
        with mock.patch.object(self.api, 'set_labels', side_effect=R.Refused('prewrite refused')):
            with self.assertRaisesRegex(R.Refused, 'prewrite refused'):
                self.runner.publish(key, gen)
        old = R.strict_json(self.journal.path.read_bytes())['generations'][key]
        self.assertTrue(old['labels_intent'])
        self.assertFalse(old['labels_done'])
        self.assertFalse(old.get('labels_aborted', False))
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])

    def test_tick_observed_labels_reconcile_lost_response_before_replacement(self):
        self.runner.apply = False
        self.runner.tick(time.monotonic() + 5400)
        old_key = self.journal.issue(432)['current_generation']
        self.runner.apply = True
        self.api.fail_labels = True
        with self.assertRaises(R.Ambiguous):
            self.runner.tick(time.monotonic() + 5400)
        saved = R.strict_json(self.journal.path.read_bytes())
        self.assertTrue(saved['generations'][old_key]['labels_intent'])
        self.assertFalse(saved['generations'][old_key]['labels_done'])
        self.api.rows[432]['body'] += ' evidence after lost response'
        self.journal.data = saved
        self.runner.tick(time.monotonic() + 5400)
        saved = R.strict_json(self.journal.path.read_bytes())
        replacement = saved['issues']['432']['current_generation']
        self.assertNotEqual(replacement, old_key)
        self.assertEqual(saved['generations'][old_key]['status'], 'stale')
        self.assertTrue(saved['generations'][old_key]['labels_done'])
        self.assertFalse(saved['generations'][old_key].get('labels_aborted', False))
        self.assertEqual(saved['generations'][replacement]['status'], 'published')
        self.assertEqual(R.consume_comment(self.api.rows[432]['comments'][0], 432, self.journal),
                         saved['generations'][replacement]['record'])
        self.assertEqual(self.tasks.calls, 2)

    def test_tick_owner_provenance_refusal_keeps_dry_run_fenced_until_resolved(self):
        self.runner.apply = False
        self.runner.tick(time.monotonic() + 5400)
        key = self.journal.issue(432)['current_generation']
        self.runner.apply = True
        self.api.rows[432]['labels'].append('size/M')
        self.runner.tick(time.monotonic() + 5400)
        saved = R.strict_json(self.journal.path.read_bytes())
        self.assertEqual(saved['generations'][key]['status'], 'dry-run')
        self.assertIn('owner-override', saved['issues']['432']['suspension'])
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        self.assertEqual(self.tasks.calls, 1)
        self.api.rows[432]['events'].append(dict(id=1, actor='owner', event='labeled', label='size/M',
                                                created_at=NOW.isoformat()))
        self.journal.data = saved
        self.runner.tick(time.monotonic() + 5400)
        self.assertEqual(self.journal.data['generations'][key]['status'], 'stale')
        replacement = self.journal.issue(432)['current_generation']
        self.assertEqual(self.journal.data['generations'][replacement]['status'], 'published')
        self.assertEqual(self.tasks.calls, 2)
        self.runner.sweep()
        self.assertIn('triage/maybe-fixed', self.api.rows[432]['labels'])

    def test_tick_ambiguous_replacement_keeps_original_until_publication_recovers(self):
        for phase in ('labels', 'comment'):
            with self.subTest(phase=phase):
                # Use a separate issue for each independent ambiguous write outcome.
                number = 432 if phase == 'labels' else 434
                self.api.rows = {number: issue(number)}
                self.api.writes = []
                self.runner.apply = False
                self.runner.tick(time.monotonic() + 5400)
                old_key = self.journal.issue(number)['current_generation']
                self.runner.apply = True
                self.api.rows[number]['body'] += ' first edit'
                setattr(self.api, 'fail_' + phase, True)
                with self.assertRaises(R.Ambiguous):
                    self.runner.tick(time.monotonic() + 5400)
                saved = R.strict_json(self.journal.path.read_bytes())
                replacement = saved['issues'][str(number)]['current_generation']
                self.assertEqual(saved['generations'][old_key]['status'], 'dry-run')
                self.assertEqual(saved['generations'][replacement]['status'], 'publishing')
                self.assertTrue(saved['generations'][replacement][phase + '_intent'])
                self.assertFalse(saved['generations'][replacement][phase + '_done'])
                writes = copy.deepcopy(self.api.writes)
                self.runner.sweep()
                self.assertEqual(self.api.writes, writes)
                self.journal.data = saved
                self.runner.tick(time.monotonic() + 5400)
                saved = R.strict_json(self.journal.path.read_bytes())
                self.assertEqual(saved['generations'][old_key]['status'], 'stale')
                self.assertEqual(saved['generations'][replacement]['status'], 'published')
                self.assertEqual(len(self.api.rows[number]['comments']), 1)
        self.assertEqual(self.tasks.calls, 4)

    def test_sweep_cannot_bypass_an_uncertain_task_or_ambiguous_publication(self):
        self.tasks.unknown = True
        self.queue()
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        key, gen = self.gen()
        gen['status'] = 'stale'
        gen['labels_intent'], gen['labels_done'] = True, False
        self.journal.save()
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])

    def test_sweep_reminder_is_suspended_for_conflicting_owner_namespace_values(self):
        self.api.rows[432].update(labels=['triage/needs-info', 'size/M', 'size/L'], events=[
            dict(id=n, actor='owner', event='labeled', label=label, created_at=NOW.isoformat())
            for n, label in ((1, 'triage/needs-info'), (2, 'size/M'), (3, 'size/L'))])
        self.journal.issue(432)['needs_since'] = NOW.isoformat()
        self.clock = NOW + dt.timedelta(days=31)
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        self.assertIn('owner-override', self.journal.issue(432)['suspension'])


class AuxiliaryRecoveryFence(Fixture):
    def reload_journal(self):
        expected = R.strict_json(self.journal.path.read_bytes())
        schema = C.Validator(ROOT / 'schemas/issue-triage-runner-v3.json')
        self.assertEqual(schema.errors(expected, 'urn:af:schema:issue-triage-runner:3'), [])
        self.journal.close()
        self.journal = R.Journal(self.root, 'owner/repo', 'runner', 'Europe/Moscow')
        self.assertEqual(self.journal.data, expected)
        self.addCleanup(self.journal.close)
        self.runner.journal = self.journal
        return self.journal.issue(432)

    def assert_aborted_with_authority(self, current, before):
        state = self.reload_journal()
        key, = state['sweeps']
        self.assertEqual(state['sweeps'][key], 'aborted')
        self.assertIn('triage/maybe-fixed', state['aux_labels'][key])
        self.assertIn('triage-sweep evidence=' + key, state['aux_bodies'][key])
        self.assertEqual(state['current_generation'], current)
        comment, = self.api.rows[432]['comments']
        self.assertEqual(R.consume_comment(comment, 432, self.journal),
                         self.journal.data['generations'][current]['record'])
        self.assertFalse(self.runner.effects_blocked(432))
        self.assertEqual(self.api.writes, before)
        return key

    def assert_fresh_sweep_after_abort(self, key):
        set_labels = self.api.set_labels
        def checked_write(*args):
            state = R.strict_json(self.journal.path.read_bytes())['issues']['432']
            self.assertEqual(state['sweeps'][key], 'label-intent')
            self.assertIsNone(state['current_generation'])
            return set_labels(*args)
        before = len(self.api.writes)
        with mock.patch.object(self.api, 'set_labels', side_effect=checked_write):
            self.runner.sweep()
        self.assertEqual([w[0] for w in self.api.writes[before:]], ['labels', 'comment'])
        self.assertEqual(self.reload_journal()['sweeps'][key], 'done')

    def test_second_guard_main_refusal_aborts_durably_preserves_authority_and_allows_fresh_sweep(self):
        self.queue()
        current = self.journal.issue(432)['current_generation']
        before = copy.deepcopy(self.api.writes)
        set_labels = self.api.set_labels
        def changed_after_intent(*args):
            state = R.strict_json(self.journal.path.read_bytes())['issues']['432']
            self.assertEqual(list(state['sweeps'].values()), ['label-intent'])
            self.assertIsNone(state['current_generation'])
            self.manifest.main = 'f' * 40
            return set_labels(*args)  # Calls the second guard before any mutation.
        with mock.patch.object(self.api, 'set_labels', side_effect=changed_after_intent):
            with self.assertRaisesRegex(R.Refused, 'sweep main changed'):
                self.runner.sweep()
        key = self.assert_aborted_with_authority(current, before)
        self.assert_fresh_sweep_after_abort(key)

    def test_first_api_prewrite_refusals_abort_durably_without_transport_or_authority_loss(self):
        self.queue()
        current = self.journal.issue(432)['current_generation']
        before = copy.deepcopy(self.api.writes)
        client = R.GitHub('owner/repo', 'fake', self.lease, time.monotonic() + 5400, 'runner')
        for refusal in (R.Refused('prewrite refused'), C.Refusal('test', 'prewrite refused')):
            with self.subTest(refusal=type(refusal).__name__):
                def refuse():
                    state = R.strict_json(self.journal.path.read_bytes())['issues']['432']
                    self.assertEqual(list(state['sweeps'].values()), ['label-intent'])
                    self.assertIsNone(state['current_generation'])
                    self.api.before_write()
                    raise refusal
                client.before_write = refuse
                with mock.patch.object(self.api, 'set_labels', side_effect=client.set_labels), \
                        mock.patch.object(client.rest, 'call', side_effect=AssertionError('transport forbidden')):
                    with self.assertRaisesRegex(type(refusal), 'prewrite refused'):
                        self.runner.sweep()
                key = self.assert_aborted_with_authority(current, before)
        self.assert_fresh_sweep_after_abort(key)

    def test_prewrite_refusal_after_a_delete_remains_ambiguous_and_fenced(self):
        client = R.GitHub('owner/repo', 'fake', self.lease, time.monotonic() + 5400, 'runner')
        def guard():
            self.api.before_write()
            if self.api.writes:
                raise C.Refusal('test', 'later prewrite refused')
        client.before_write = guard
        def transport(method, path, **kwargs):
            if method == 'GET':
                self.assertEqual(path, '/user')
                return 200, dict(login='runner')
            self.assertEqual((method, path), ('DELETE', '/repos/owner/repo/issues/432/labels/triage%2Fpending'))
            self.api.writes.append((method, path))
            self.api.rows[432]['labels'].remove('triage/pending')
            return 204, None
        with mock.patch.object(self.api, 'set_labels', side_effect=client.set_labels), \
                mock.patch.object(client.rest, 'call', side_effect=transport):
            with self.assertRaisesRegex(R.Ambiguous, 'partial label write'):
                self.runner.sweep()
        state = self.reload_journal()
        key, = state['sweeps']
        self.assertEqual(state['sweeps'][key], 'label-intent')
        self.assertIsNone(state['current_generation'])
        self.assertTrue(self.runner.effects_blocked(432))
        before = copy.deepcopy(self.api.writes)
        self.runner.sweep()
        self.assertEqual(self.api.writes, before)
        self.assertEqual(state['sweeps'][key], 'label-intent')

    def test_read_refusal_after_labels_return_does_not_abort_the_effect(self):
        label_written = False
        read = self.api.issue
        def observed(number):
            R.require(not label_written, 'postwrite read refused')
            return read(number)
        set_labels = self.api.set_labels
        def written(*args):
            nonlocal label_written
            set_labels(*args)
            label_written = True
        with mock.patch.object(self.api, 'issue', side_effect=observed), \
                mock.patch.object(self.api, 'set_labels', side_effect=written):
            with self.assertRaisesRegex(R.Refused, 'postwrite read refused'):
                self.runner.sweep()
        state = self.reload_journal()
        key, = state['sweeps']
        self.assertEqual(state['sweeps'][key], 'label-intent')
        self.assertIsNone(state['current_generation'])
        self.assertTrue(self.runner.effects_blocked(432))
        self.assertEqual([w[0] for w in self.api.writes], ['labels'])

    def test_exact_existing_principal_comment_is_adopted_with_body_and_no_api_writes(self):
        evidence, = R.sweep_evidence(self.manifest.citations(432), [], 432)[0]
        key = R.digest(evidence)
        body = ('Possible completed fix; owner review required.\n\n' + evidence['citation']
                + '\n\ntriage-sweep evidence=' + key)
        self.api.rows[432]['comments'].append(dict(id=100, author='runner', body=body,
                                                   created_at=NOW.isoformat()))
        self.assertEqual(self.journal.issue(432)['sweeps'], {})
        self.runner.sweep()
        state = self.reload_journal()
        self.assertEqual(state['sweeps'], {key: 'done'})
        self.assertEqual(state['aux_bodies'], {key: body})
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        self.assertEqual(len(self.api.rows[432]['comments']), 1)
        self.assertEqual(self.tasks.calls, 0)

    def test_partial_delete_or_unknown_post_fences_sweep_and_queued_canonical_until_observed(self):
        for lost in ('DELETE-before', 'DELETE-after', 'POST-before'):
            with self.subTest(lost=lost):
                self.api.rows = {432: issue()}
                self.api.writes.clear()
                self.journal.data['issues'].clear()
                self.journal.data['generations'].clear()
                self.journal.data['ledger'].clear()
                self.journal.data['events'].clear()
                client = R.GitHub('owner/repo', 'fake', self.lease, time.monotonic() + 5400, 'runner')
                client.before_write = lambda: self.api.before_write()
                desired = ['bug', 'outside', 'triage/maybe-fixed']
                def transport(method, path, **kwargs):
                    if method == 'GET':
                        self.assertEqual(path, '/user')
                        return 200, dict(login='runner')
                    saved = R.strict_json(self.journal.path.read_bytes())['issues']['432']
                    key = next(iter(saved['sweeps']))
                    self.assertEqual(saved['sweeps'][key], 'label-intent')
                    self.assertEqual(saved['aux_labels'][key], desired)
                    self.assertIn('triage-sweep evidence=' + key, saved['aux_bodies'][key])
                    self.assertIsNone(saved['current_generation'])
                    self.api.writes.append((method, path))
                    if lost == method + '-before':
                        raise OSError('response lost; outcome unknown')
                    self.assertEqual((method, path), ('DELETE', '/repos/owner/repo/issues/432/labels/triage%2Fpending'))
                    self.api.rows[432]['labels'].remove('triage/pending')
                    self.api.rows[432]['events'].append(dict(id=1, actor='runner', event='unlabeled',
                                                             label='triage/pending', created_at=NOW.isoformat()))
                    if lost == 'DELETE-after':
                        raise OSError('DELETE applied; response lost')
                    return 204, None
                with mock.patch.object(self.api, 'set_labels', side_effect=client.set_labels), \
                        mock.patch.object(client.rest, 'call', side_effect=transport):
                    with self.assertRaisesRegex(R.Ambiguous, 'API write outcome unknown'):
                        self.runner.sweep()
                state = self.reload_journal()
                key = next(iter(state['sweeps']))
                before = copy.deepcopy(self.api.writes)
                self.runner.sweep()
                self.assertEqual(state['sweeps'][key], 'label-intent')
                with self.assertRaisesRegex(R.Refused, 'earlier issue generation has unresolved effects'):
                    self.queue('after-unknown-' + lost)
                gen_key, gen = self.gen()
                self.assertEqual(gen['status'], 'checked')
                # Neither a different evidence identity nor unrelated observed labels can
                # replace/complete the retained intent, even through a direct auxiliary call.
                self.api.rows[432]['labels'].append('unrelated')
                self.runner.reconcile_auxiliary(self.api.issue(432), state)
                with self.assertRaisesRegex(R.Refused, 'unresolved effects'):
                    self.runner.auxiliary(self.api.issue(432), state['sweeps'], 'different', 'different effect')
                self.runner.sweep()
                self.assertEqual(self.api.writes, before)
                self.assertEqual(state['sweeps'], {key: 'label-intent'})
                self.assertEqual(state['aux_labels'][key], desired)
                self.assertEqual(self.api.rows[432]['comments'], [])

                # Establish the exact recorded outcome without a runner retry. Observation
                # completes only the label phase and performs no public write itself.
                self.api.rows[432]['labels'] = list(desired)
                self.api.rows[432]['events'].append(dict(id=2, actor='runner', event='labeled',
                                                         label='triage/maybe-fixed', created_at=NOW.isoformat()))
                self.runner.reconcile_auxiliary(self.api.issue(432), state)
                self.assertEqual(state['sweeps'][key], 'labels-done')
                self.assertEqual(self.api.writes, before)
                # The queued proposal used the partial labels and now fails freshness.
                with self.assertRaisesRegex(R.Refused, 'labels changed'):
                    self.runner.publish(gen_key, gen)
                self.runner.sweep()
                self.assertEqual(state['sweeps'][key], 'done')
                self.assertEqual(self.api.writes[len(before):], [('comment', 432, state['aux_bodies'][key], None)])
                self.assertEqual(len(self.api.rows[432]['comments']), 1)
                self.queue('after-observed-' + lost)
                self.assertEqual(self.journal.data['generations'][state['published']]['status'], 'published')

    def test_returned_partial_labels_remain_fenced_and_matching_comment_cannot_complete_them(self):
        def partial(number, desired, current):
            self.api.before_write()
            self.api.rows[number]['labels'].remove('triage/pending')
            self.api.writes.append(('partial-labels', number))
        with mock.patch.object(self.api, 'set_labels', side_effect=partial):
            with self.assertRaisesRegex(R.Refused, 'auxiliary label write not observed'):
                self.runner.sweep()
        state = self.reload_journal()
        key = next(iter(state['sweeps']))
        self.api.rows[432]['comments'].append(dict(id=100, author='runner', body=state['aux_bodies'][key],
                                                  created_at=NOW.isoformat()))
        before = copy.deepcopy(self.api.writes)
        self.runner.sweep()
        self.assertEqual(state['sweeps'][key], 'label-intent')
        self.assertTrue(self.runner.effects_blocked(432))
        self.assertEqual(self.api.writes, before)

    def test_lost_completed_labels_are_observed_before_comment_intent_and_never_repeated(self):
        self.api.fail_labels = True
        with self.assertRaisesRegex(R.Ambiguous, 'lost label response'):
            self.runner.sweep()
        state = self.reload_journal()
        key = next(iter(state['sweeps']))
        self.assertEqual(state['sweeps'][key], 'label-intent')
        self.runner.reconcile_auxiliary(self.api.issue(432), state)
        state = self.reload_journal()
        self.assertEqual(state['sweeps'][key], 'labels-done')
        comment = self.api.comment
        def observed_comment(*args):
            saved = R.strict_json(self.journal.path.read_bytes())['issues']['432']
            self.assertEqual(saved['sweeps'][key], 'comment-intent')
            self.assertEqual(sorted(self.api.rows[432]['labels']), saved['aux_labels'][key])
            return comment(*args)
        self.api.fail_comment = True
        with mock.patch.object(self.api, 'comment', side_effect=observed_comment):
            with self.assertRaisesRegex(R.Ambiguous, 'lost comment response'):
                self.runner.sweep()
        state = self.reload_journal()
        self.assertEqual(state['sweeps'][key], 'comment-intent')
        self.runner.sweep()
        self.assertEqual([w[0] for w in self.api.writes], ['labels', 'comment'])
        self.assertEqual(state['sweeps'][key], 'done')

    def test_success_observes_exact_labels_before_advancing_to_comment_intent(self):
        read = self.api.issue
        observed = []
        def observe(number):
            row = read(number)
            saved = R.strict_json(self.journal.path.read_bytes())['issues'].get(str(number), {})
            for key, phase in saved.get('sweeps', {}).items():
                if phase == 'label-intent' and sorted(row['labels']) == saved['aux_labels'][key]:
                    observed.append(key)
            return row
        comment = self.api.comment
        def checked_comment(*args):
            self.assertEqual(len(observed), 1)
            saved = R.strict_json(self.journal.path.read_bytes())['issues']['432']
            self.assertEqual(saved['sweeps'][observed[0]], 'comment-intent')
            return comment(*args)
        with mock.patch.object(self.api, 'issue', side_effect=observe), \
                mock.patch.object(self.api, 'comment', side_effect=checked_comment):
            self.runner.sweep()
        state = self.reload_journal()
        self.assertEqual(state['sweeps'], {observed[0]: 'done'})
        self.assertEqual([w[0] for w in self.api.writes], ['labels', 'comment'])

    def test_reader_refuses_label_checkpoint_without_its_exact_desired_state(self):
        self.api.fail_labels = True
        with self.assertRaises(R.Ambiguous):
            self.runner.sweep()
        saved = R.strict_json(self.journal.path.read_bytes())
        self.journal.close()
        for phase in ('label-intent', 'labels-done', 'aborted'):
            for field, error in (('aux_labels', 'auxiliary label intent has no desired labels'),
                                 ('aux_bodies', 'auxiliary checkpoint has no recorded body')):
                with self.subTest(phase=phase, field=field):
                    broken = copy.deepcopy(saved)
                    state = broken['issues']['432']
                    key, = state['sweeps']
                    state['sweeps'][key] = phase
                    state[field].clear()
                    self.journal.path.write_bytes(R.encoded(broken))
                    before = self.journal.path.read_bytes()
                    with self.assertRaisesRegex(R.Refused, error):
                        R.Journal(self.root, 'owner/repo', 'runner', 'Europe/Moscow')
                    self.assertEqual(self.journal.path.read_bytes(), before)

    def test_intent_fsync_failure_prevents_label_call_and_preserves_canonical_authority(self):
        self.queue()
        state = self.journal.issue(432)
        current = state['current_generation']
        before = copy.deepcopy(self.api.writes)
        with mock.patch.object(R.os, 'fsync', side_effect=OSError('intent not durable')):
            with self.assertRaises(OSError):
                self.runner.auxiliary(self.api.issue(432), state['sweeps'], 'new-evidence',
                                      'possible fix', 'triage/maybe-fixed', self.main)
        saved = R.strict_json(self.journal.path.read_bytes())['issues']['432']
        self.assertEqual(saved['current_generation'], current)
        self.assertEqual(state['current_generation'], current)
        self.assertNotIn('new-evidence', saved['sweeps'])
        self.assertEqual(self.api.writes, before)

    def test_auxiliary_creation_reconciles_even_when_evidence_is_no_longer_proposed(self):
        self.api.fail_comment = True
        with self.assertRaises(R.Ambiguous):
            self.runner.sweep()
        with mock.patch.object(self.manifest, 'citations', return_value=[]):
            self.runner.sweep()
        self.assertEqual(len(self.api.rows[432]['comments']), 1)
        self.assertTrue(all(v == 'done' for v in self.journal.issue(432)['sweeps'].values()))
        self.assertEqual(len([w for w in self.api.writes if w[0] == 'comment']), 1)

    def test_unobserved_auxiliary_create_blocks_canonical_publication(self):
        self.api.fail_before_comment = True
        with self.assertRaises(R.Ambiguous):
            self.runner.sweep()
        before = copy.deepcopy(self.api.writes)
        with self.assertRaises(R.Refused):
            self.queue('after-unknown-sweep')
        self.assertEqual(self.api.writes, before)
        self.assertFalse(self.api.rows[432]['comments'])


class ApiWallFence(Fixture):
    def test_batch_wall_during_http_mutation_retains_ambiguous_write_authority(self):
        client = R.GitHub('owner/repo', 'fake', self.lease, time.monotonic() + 5400, 'runner')
        with mock.patch.object(client.rest, 'call', side_effect=[(200, dict(login='runner')),
                                                                R.Refused('batch wall reached')]) as transport:
            with self.assertRaises(R.Ambiguous):
                client.comment(432, 'not repeated')
        self.assertEqual([c.args[0] for c in transport.call_args_list], ['GET', 'POST'])


class PrincipalFence(Fixture):
    def test_principal_change_is_rechecked_before_each_public_mutation(self):
        client = R.GitHub('owner/repo', 'fake', self.lease, time.monotonic() + 5400, 'runner')
        with mock.patch.object(client.rest, 'call', return_value=(200, dict(login='different'))) as transport:
            with self.assertRaisesRegex(R.Refused, 'principal changed'):
                client.comment(432, 'not sent')
        self.assertEqual(transport.call_count, 1)
        self.assertEqual(transport.call_args.args[:2], ('GET', '/user'))


class DryRunProgress(Fixture):
    def test_dry_ticks_progress_past_four_issues_and_write_transition_rechecks_without_replanning(self):
        self.api.rows = {n: issue(n) for n in range(1, 7)}
        self.runner.apply = False
        for expected in (4, 6, 6):
            self.runner.tick(time.monotonic() + 5400)
            self.assertEqual(self.tasks.calls, expected)
            self.assertEqual(self.api.writes, [])
        self.assertEqual(len(self.output), 6)
        self.runner.apply = True
        self.runner.tick(time.monotonic() + 5400)
        self.assertEqual(sum(len(i['comments']) for i in self.api.rows.values()), 4)
        self.runner.tick(time.monotonic() + 5400)
        self.assertEqual(sum(len(i['comments']) for i in self.api.rows.values()), 6)
        self.assertEqual(self.tasks.calls, 6)


class ReminderBootstrap(Fixture):
    def test_existing_needs_info_reminder_uses_trusted_label_time_without_model(self):
        self.api.rows[432]['labels'] = ['triage/needs-info']
        self.api.rows[432]['events'] = [dict(id=1, actor='owner', event='labeled', label='triage/needs-info',
                                               created_at=NOW.isoformat())]
        self.clock = NOW + dt.timedelta(days=29)
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        self.clock = NOW + dt.timedelta(days=30)
        self.runner.sweep()
        self.runner.sweep()
        self.assertEqual(len(self.api.rows[432]['comments']), 1)
        self.assertEqual(self.tasks.calls, 0)

    def test_existing_needs_info_without_a_known_request_time_never_guesses_age(self):
        self.api.rows[432]['labels'] = ['triage/needs-info']
        self.clock = NOW + dt.timedelta(days=90)
        self.runner.sweep()
        self.assertEqual(self.api.writes, [])
        self.assertIsNone(self.journal.issue(432)['needs_since'])


class JournalReader(Fixture):
    def test_current_schema_identity_and_aborted_checkpoint_refuse_retired_shapes(self):
        self.assertEqual(R.STATE_SCHEMA, 'af.issue-triage-runner/3')
        path = ROOT / 'schemas/issue-triage-runner-v3.json'
        schema = json.loads(path.read_text(encoding='utf-8'))
        self.assertEqual(schema['$schema'], 'https://json-schema.org/draft/2020-12/schema')
        self.assertEqual(schema['$id'], 'urn:af:schema:issue-triage-runner:3')
        self.assertEqual(schema['properties']['schema']['const'], R.STATE_SCHEMA)
        validator = C.Validator(path)
        state = self.journal.issue(432)
        state['sweeps']['evidence'] = 'aborted'
        state['aux_bodies']['evidence'] = 'recorded auxiliary body'
        state['aux_labels']['evidence'] = ['triage/maybe-fixed']
        self.journal.save()
        doc = R.strict_json(self.journal.path.read_bytes())
        self.assertEqual(validator.errors(doc, 'urn:af:schema:issue-triage-runner:3'), [])
        for version in (1, 2):
            doc['schema'] = 'af.issue-triage-runner/%d' % version
            self.assertTrue(validator.errors(doc, 'urn:af:schema:issue-triage-runner:3'))
        # Phase validation remains closed independently of the version discriminator.
        doc['schema'] = R.STATE_SCHEMA
        doc['issues']['432']['sweeps']['evidence'] = 'unknown'
        errors = validator.errors(doc, 'urn:af:schema:issue-triage-runner:3')
        self.assertEqual(len(errors), 1)
        self.assertIn('$.issues.432.sweeps.evidence: must be one of', errors[0])
        doc['issues']['432']['sweeps']['evidence'] = 'done'
        self.assertEqual(validator.errors(doc, 'urn:af:schema:issue-triage-runner:3'), [])

    def test_v3_reader_round_trips_every_current_auxiliary_checkpoint(self):
        self.queue()
        state = self.journal.issue(432)
        for phase in ('label-intent', 'labels-done', 'aborted', 'comment-intent', 'done'):
            state['sweeps'][phase] = phase
            state['aux_bodies'][phase] = 'recorded body for ' + phase
            if phase in ('label-intent', 'labels-done', 'aborted'):
                state['aux_labels'][phase] = ['triage/maybe-fixed']
        self.journal.save()
        expected = copy.deepcopy(self.journal.data)
        before = self.journal.path.read_bytes()
        schema = C.Validator(ROOT / 'schemas/issue-triage-runner-v3.json')
        self.assertEqual(schema.errors(expected, 'urn:af:schema:issue-triage-runner:3'), [])
        self.journal.close()
        with_reader = R.Journal(self.root, 'owner/repo', 'runner', 'Europe/Moscow')
        self.addCleanup(with_reader.close)
        self.assertEqual(with_reader.data, expected)
        self.assertEqual(self.journal.path.read_bytes(), before)

    def test_real_reader_round_trips_current_writer_and_refuses_prior_versions(self):
        self.queue()
        expected = R.strict_json(self.journal.path.read_bytes())
        self.journal.close()
        reader = R.Journal(self.root, 'owner/repo', 'runner', 'Europe/Moscow')
        self.assertEqual(reader.data, expected)
        reader.close()
        schema = C.Validator(ROOT / 'schemas/issue-triage-runner-v3.json')
        for version in (1, 2, 0):
            with self.subTest(version=version):
                prior = copy.deepcopy(expected)
                prior['schema'] = 'af.issue-triage-runner/%d' % version
                if version == 1:
                    for state in prior['issues'].values():
                        state.pop('aux_labels')
                self.assertTrue(schema.errors(prior, 'urn:af:schema:issue-triage-runner:3'))
                self.journal.path.write_bytes(R.encoded(prior))
                before = self.journal.path.read_bytes()
                with self.assertRaisesRegex(R.Refused, 'journal schema refused'):
                    R.Journal(self.root, 'owner/repo', 'runner', 'Europe/Moscow')
                self.assertEqual(self.journal.path.read_bytes(), before)

    def test_source_schema_parity_matches_the_report_profile_bounds(self):
        schema = C.Validator(ROOT / 'schemas/report-sources-v1.json')
        self.assertEqual(schema.errors(self.bundle(), 'urn:af:schema:report-sources:1'), [])

    def test_unknown_after_settlement_restores_full_reservation_and_blocks_admission(self):
        bundle = self.bundle()
        key, _ = self.journal.claim(R.generation_identity(432, bundle, self.main, 'known'), bundle, issue())
        self.journal.reserve(key, NOW)
        self.journal.settle(key, dict(chargeable_tokens='1', phase=dict(kind='finished')))
        self.journal.settle(key, dict(chargeable_tokens='1', unknown_usage_attempts=1, phase=dict(kind='finished')))
        self.assertIsNone(self.journal.data['ledger'][key]['settled'])
        self.assertIsNotNone(self.journal.data['blocked'])


class NightlyLease(Fixture):
    def test_nightly_and_runner_use_identical_required_anchor_and_fence(self):
        builder = mock.Mock()
        builder.main.return_value = 3
        lease = mock.Mock(uncertain=False, expected_identity=[1, 2])
        with mock.patch.object(R.host_lock, 'read_anchor', return_value=[1, 2]) as anchor, \
                mock.patch.object(R.host_lock, 'MachineLock', return_value=lease) as lock, \
                mock.patch.object(R, 'NightlyHost') as host:
            result = R.nightly_build_main(['--config', '/operator/nightly.toml'], builder=builder)
        self.assertEqual(result, 3)
        anchor.assert_called_once_with(required=True)
        lock.assert_called_once_with(expected_identity=[1, 2])
        lease.guard.assert_called_once()
        host.return_value.clean.assert_called_once()
        lease.confirm_clean.assert_called_once()
        lease.release.assert_called_once()
        builder.main.assert_called_once_with(['build', '--config', '/operator/nightly.toml',
                                              '--night-of', 'af-nightly-build.service'], host=host.return_value)

    def test_nightly_uncertain_cleanup_keeps_fence_and_no_retry(self):
        builder = mock.Mock()
        builder.main.return_value = 0
        lease = Lease()
        with mock.patch.object(R.host_lock, 'read_anchor', return_value=[1, 2]), \
                mock.patch.object(R.host_lock, 'MachineLock', return_value=lease), \
                mock.patch.object(R, 'NightlyHost') as host:
            host.return_value.clean.side_effect = R.Refused('unconfirmed container cleanup')
            with self.assertRaises(R.Refused):
                R.nightly_build_main(['--config', '/operator/nightly.toml'], builder=builder)
        self.assertTrue(lease.uncertain)
        self.assertFalse(lease.cleaned)
        self.assertEqual(builder.main.call_count, 1)

    def test_nightly_container_cleanup_only_names_the_owned_container(self):
        processes = mock.Mock()
        name = 'af-nightly-2026-11-02-build-x86_64-unknown-linux-musl'
        processes.run.side_effect = [b'foreign-container\n', subprocess.CompletedProcess([], 0, b'', b''),
                                     (name + '\nforeign-container\n').encode(), b'', b'foreign-container\n']
        host = R.NightlyHost(processes, self.lease)
        client_env = {'PATH': '/pinned/docker/bin', 'LC_ALL': 'C'}
        host.run(['docker', 'run', '--name', name, 'pinned-image'], timeout=60, env=client_env)
        with mock.patch.object(host, 'confirm_cgroup'):
            host.clean()
        calls = [c.args[0] for c in processes.run.call_args_list]
        self.assertIn(['docker', 'rm', '--force', name], calls)
        self.assertNotIn(['docker', 'rm', '--force', 'foreign-container'], calls)
        self.assertTrue(all(c.kwargs.get('env') == client_env for c in processes.run.call_args_list))

    def test_nightly_native_descendants_and_missing_cgroup_confirmation_keep_fence(self):
        membership = self.root / 'membership'
        membership.write_text('0::/system.slice/af-nightly-build.service\n')
        cgroup_root = self.root / 'cgroups'
        unit = cgroup_root / 'system.slice/af-nightly-build.service'
        unit.mkdir(parents=True)
        (unit / 'cgroup.procs').write_text(str(os.getpid()) + '\n')
        host = R.NightlyHost(mock.Mock(), self.lease)
        host.confirm_cgroup(membership, cgroup_root)
        child = unit / 'native-child'
        child.mkdir()
        (child / 'cgroup.procs').write_text('99999999\n')
        with self.assertRaisesRegex(R.Refused, 'descendants remain'):
            host.confirm_cgroup(membership, cgroup_root)
        with mock.patch.object(host, 'confirm_cgroup', side_effect=R.Refused('unconfirmed')):
            with self.assertRaises(R.Refused):
                host.clean()
        self.assertTrue(self.lease.uncertain)
        self.lease.uncertain = False

    def test_staged_schedule_is_half_hour_dry_run_and_shared_nightly_wiring(self):
        service = ROOT / 'fixtures/issue-triage/service'
        timer = (service / 'af-backlog-triage.timer').read_text()
        self.assertIn('OnCalendar=*-*-* *:00,30:00 UTC', timer)
        unit = (service / 'af-backlog-triage.service').read_text()
        self.assertNotIn('--apply', unit)
        self.assertIn('PrivateTmp=no', unit)
        self.assertIn('KillMode=control-group', unit)
        self.assertIn('ConditionHost=claw-3', unit)
        self.assertIn('LoadCredential=triage-github:/etc/af-triage/github-token', unit)
        self.assertIn('${CREDENTIALS_DIRECTORY}/triage-telegram', unit)
        dropin = (service / 'nightly-lease.conf').read_text()
        self.assertIn('backlog-triage.py nightly-build', dropin)
        self.assertIn('--config /etc/af-nightly/nightly.toml', dropin)


if __name__ == '__main__':
    unittest.main()
