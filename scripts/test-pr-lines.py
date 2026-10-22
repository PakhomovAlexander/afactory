#!/usr/bin/env python3
"""Exercise `pr-lines.py`, and the figure check of `check-pr-report.py`, on a local repository
(ADR-0195): no network, no GitHub."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent
TOOL = SCRIPTS / 'pr-lines.py'
CHECK = SCRIPTS / 'check-pr-report.py'
REPORT = (SCRIPTS.parent / 'fixtures' / 'task-report' / 'report.md').read_text(encoding='utf-8')


def load():
    spec = importlib.util.spec_from_file_location('pr_lines', TOOL)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


lines = load()
GIT_ENV = {
    **os.environ, 'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_CONFIG_NOSYSTEM': '1',
    'GIT_AUTHOR_NAME': 'Fixture', 'GIT_AUTHOR_EMAIL': 'fixture@example.invalid',
    'GIT_COMMITTER_NAME': 'Fixture', 'GIT_COMMITTER_EMAIL': 'fixture@example.invalid',
}


def git(repo, *args):
    return subprocess.run(['git', '-C', str(repo), *args], check=True, capture_output=True,
                          text=True, env=GIT_ENV).stdout


def write(repo, path, text):
    target = repo / path
    target.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(text, bytes):
        target.write_bytes(text)
    else:
        target.write_text(text, encoding='utf-8')


class Figures(unittest.TestCase):
    def test_a_count_is_exact_then_rounded(self):
        for n, shown in [(0, '0'), (7, '7'), (999, '999'), (1000, '1k'), (1049, '1k'),
                         (1050, '1.1k'), (4980, '5k'), (7887, '7.9k'), (9949, '9.9k'),
                         (9950, '10k'), (10_000, '10k'), (10_499, '10k'), (10_500, '11k'),
                         (23_286, '23k'), (123_456, '123k'), (2_500_000, '2500k')]:
            self.assertEqual(lines.figure(n), shown, n)

    def test_a_path_counts_under_its_first_directory_tests_or_its_own_name(self):
        for path, name in [('crates/af/src/lib.rs', 'crates'), ('docs/telemetry.md', 'docs'),
                           ('crates/af/tests/it/x.rs', 'tests'), ('tests/smoke.rs', 'tests'),
                           ('scripts/performance/tests/test_x.py', 'tests'),
                           ('crates/tests-helper/src/a.rs', 'crates'), ('Cargo.lock', 'Cargo.lock'),
                           ('.github/workflows/ci.yml', '.github'), ('docs/tests', 'docs')]:
            self.assertEqual(lines.group(path), name, path)

    def test_the_line_is_largest_first_and_none_when_nothing_was_added(self):
        self.assertEqual(lines.render({}), 'Lines added: none')
        self.assertEqual(lines.render({'docs': 10_000, 'tests': 5000, 'crates': 12_345, 'b': 3,
                                       'a': 3}),
                         'Lines added: crates/12k docs/10k tests/5k a/3 b/3')
        self.assertEqual(lines.block({'docs': 12}),
                         '<!-- af-pr-lines:v1 -->\nLines added: docs/12\n<!-- /af-pr-lines -->\n')

    def test_the_line_parses_back_in_any_order_and_refuses_other_shapes(self):
        self.assertEqual(lines.parse('Lines added: docs/10k tests/5k'),
                         {'docs': '10k', 'tests': '5k'})
        self.assertEqual(lines.parse('  Lines added: tests/5k docs/10k  '),
                         {'docs': '10k', 'tests': '5k'})
        self.assertEqual(lines.parse('Lines added: none'), {})
        self.assertEqual(lines.parse('Lines added: .github/412 Cargo.lock/17 x/1.5k'),
                         {'.github': '412', 'Cargo.lock': '17', 'x': '1.5k'})
        for bad in ['Lines added:', 'Lines added: docs', 'Lines added: docs/', 'Lines added: /5k',
                    'Lines added: docs/5kk', 'Lines added: docs/05', 'Lines added: docs/1.25k',
                    'Lines added: docs/5k docs/6k', 'Lines: docs/5k', 'docs/5k',
                    'Lines added: docs/5k, tests/1k', 'Lines added: docs/5000']:
            self.assertIsNone(lines.parse(bad), bad)


class Repository(unittest.TestCase):
    """A base commit and a branch over it that adds, moves, deletes and binary-changes files."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name) / 'repo'
        self.repo.mkdir()
        git(self.repo, 'init', '-q', '-b', 'main')
        write(self.repo, 'README.md', 'one\n')
        write(self.repo, 'crates/af/src/lib.rs', ''.join(f'line {n}\n' for n in range(50)))
        write(self.repo, 'crates/af/src/old.rs', ''.join(f'old {n}\n' for n in range(40)))
        write(self.repo, 'docs/guide.md', 'a\nb\n')
        write(self.repo, 'docs/gone.md', 'x\ny\nz\n')
        write(self.repo, 'fixtures/image.png', b'\x89PNG\r\n\x1a\n\x00\x01')
        git(self.repo, 'add', '-A')
        git(self.repo, 'commit', '-qm', 'base')
        self.base = git(self.repo, 'rev-parse', 'HEAD').strip()
        git(self.repo, 'checkout', '-qb', 'feature')
        write(self.repo, 'README.md', 'one\ntwo\nthree\n')
        write(self.repo, 'crates/af/src/lib.rs',
              ''.join(f'line {n}\n' for n in range(50)) + ''.join(f'new {n}\n' for n in range(120)))
        write(self.repo, 'crates/af/tests/it/main.rs', ''.join(f'test {n}\n' for n in range(30)))
        # A move with one changed line adds one line, not forty.
        git(self.repo, 'mv', 'crates/af/src/old.rs', 'crates/af/src/moved.rs')
        write(self.repo, 'crates/af/src/moved.rs',
              ''.join(f'old {n}\n' for n in range(40)).replace('old 3\n', 'old three\n'))
        write(self.repo, 'docs/guide.md', 'a\nb\nc\nd\ne\n')
        (self.repo / 'docs' / 'gone.md').unlink()
        write(self.repo, 'fixtures/image.png', b'\x89PNG\r\n\x1a\n\x00\x02\x03')
        write(self.repo, 'scripts/performance/tests/test_new.py', 'assert True\n')
        git(self.repo, 'add', '-A')
        git(self.repo, 'commit', '-qm', 'feature')
        self.head = git(self.repo, 'rev-parse', 'HEAD').strip()
        # Main moves on after the fork: a three-dot diff does not count that.
        git(self.repo, 'checkout', '-q', 'main')
        write(self.repo, 'docs/later.md', ''.join('later\n' for _ in range(500)))
        git(self.repo, 'add', '-A')
        git(self.repo, 'commit', '-qm', 'later on main')
        git(self.repo, 'checkout', '-q', 'feature')

    def tearDown(self):
        self.tmp.cleanup()

    def run_tool(self, *args):
        return subprocess.run([sys.executable, '-I', str(TOOL), '--repo', str(self.repo), *args],
                              capture_output=True, text=True, env=GIT_ENV)

    def test_the_counts_follow_the_rules(self):
        self.assertEqual(lines.counts(lines.numstat(self.repo, 'main', 'feature')), {
            'README.md': 2, 'crates': 121, 'tests': 31, 'docs': 3})
        self.assertEqual(lines.counts(lines.numstat(self.repo, self.base, self.head)), {
            'README.md': 2, 'crates': 121, 'tests': 31, 'docs': 3})
        entries = {path: (added, deleted) for added, deleted, path
                   in lines.numstat(self.repo, 'main', 'feature')}
        self.assertEqual(entries['fixtures/image.png'], (None, None), 'binary')
        self.assertEqual(entries['crates/af/src/moved.rs'], (1, 1), 'the rename, under its new path')
        self.assertNotIn('crates/af/src/old.rs', entries)
        self.assertEqual(entries['docs/gone.md'], (0, 3))

    def test_the_tool_prints_the_block_and_json(self):
        done = self.run_tool('--base', 'main', '--head', 'feature')
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(done.stdout, '<!-- af-pr-lines:v1 -->\n'
                         'Lines added: crates/121 tests/31 docs/3 README.md/2\n'
                         '<!-- /af-pr-lines -->\n')
        done = self.run_tool('--base', 'main', '--json')
        self.assertEqual(json.loads(done.stdout), {'README.md': 2, 'crates': 121, 'tests': 31,
                                                   'docs': 3})
        done = self.run_tool('--base', 'main', '--head', 'main')
        self.assertEqual(done.stdout, '<!-- af-pr-lines:v1 -->\nLines added: none\n'
                         '<!-- /af-pr-lines -->\n')
        done = self.run_tool('--base', 'no-such-ref')
        self.assertEqual(done.returncode, 1)
        self.assertIn('git diff no-such-ref...HEAD failed', done.stderr)

    def check(self, body, *args, event=None):
        with tempfile.TemporaryDirectory() as tmp:
            if event is None:
                path = Path(tmp) / 'body.md'
                path.write_text(body, encoding='utf-8')
                source = ['--body', str(path)]
            else:
                path = Path(tmp) / 'event.json'
                path.write_text(json.dumps(event), encoding='utf-8')
                source = ['--event', str(path)]
            done = subprocess.run([sys.executable, '-I', str(CHECK), *source, '--repo',
                                   str(self.repo), *args], capture_output=True, text=True,
                                  env=GIT_ENV)
            return done.returncode, done.stdout + done.stderr

    def test_the_check_compares_the_figures_with_the_commits(self):
        right = lines.block({'README.md': 2, 'crates': 121, 'tests': 31, 'docs': 3})
        reordered = right.replace('crates/121 tests/31 docs/3 README.md/2',
                                  'docs/3 README.md/2 tests/31 crates/121')
        stale = right.replace('crates/121', 'crates/120')
        body = lambda block: ('## What\n\nX.\n\n## af task report\n\n' + REPORT
                              + '\n## Lines added\n\n' + block + '\n')
        code, said = self.check(body(right), '--diff', 'main', 'feature')
        self.assertEqual(code, 0, said)
        self.assertIn('the figures match the pull request', said)
        code, said = self.check(body(reordered), '--diff', 'main', 'feature')
        self.assertEqual(code, 0, said)
        code, said = self.check(body(stale), '--diff', 'main', 'feature')
        self.assertEqual(code, 1, said)
        self.assertIn('expected `Lines added: crates/121 tests/31 docs/3 README.md/2`', said)
        code, said = self.check(body(stale))
        self.assertEqual(code, 0, 'without --diff the shape alone is checked:\n' + said)
        self.assertIn('not compared', said)
        code, said = self.check(body(right), '--diff', 'main', 'no-such-ref')
        self.assertEqual(code, 1, said)
        self.assertIn('could not count the lines added', said)
        # The event names the commits; the job fetched them, so the check reads them.
        event = {'action': 'synchronize', 'pull_request': {
            'body': body(right), 'user': {'login': 'contributor'},
            'head': {'ref': 'feature', 'sha': self.head, 'repo': {'full_name': 'o/r'}},
            'base': {'ref': 'main', 'sha': self.base, 'repo': {'full_name': 'o/r'}}}}
        code, said = self.check(None, event=event)
        self.assertEqual(code, 0, said)
        self.assertIn('the figures match the pull request', said)
        event['pull_request']['body'] = body(stale)
        code, said = self.check(None, event=event)
        self.assertEqual(code, 1, said)
        self.assertIn('expected `Lines added: crates/121', said)
        event['pull_request']['head']['sha'] = '0' * 40
        code, said = self.check(None, event=event)
        self.assertEqual(code, 1, said)
        self.assertIn('could not count the lines added', said)


if __name__ == '__main__':
    unittest.main()
