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

    def test_a_bar_is_the_count_against_the_largest_in_eighths_and_a_share_a_percent(self):
        self.assertEqual(lines.bar(12_345, 12_345), '████████████')
        self.assertEqual(lines.bar(6172, 12_345), '██████')
        self.assertEqual(lines.bar(1, 12_345), '▏', 'a sliver at least')
        self.assertEqual(lines.bar(10_000, 12_345), '█████████▊')
        for n, total, shown in [(1, 3, '33%'), (2, 3, '67%'), (1, 200, '1%'), (1, 201, '<1%'),
                                (12_345, 12_345, '100%')]:
            self.assertEqual(lines.share(n, total), shown, (n, total))

    def test_the_block_is_a_title_and_a_table_largest_first_or_none(self):
        self.assertEqual(lines.render({}), '**Lines added: none**')
        self.assertEqual(lines.render({'docs': 10_000, 'tests': 5000, 'crates': 12_345, 'b': 3,
                                       'a': 3}), '\n'.join([
            '**Lines added: 27k**',
            '',
            '| Directory | Added | Share |',
            '|:--|--:|:--|',
            '| crates | 12k | ████████████ 45% |',
            '| docs | 10k | █████████▊ 37% |',
            '| tests | 5k | ████▉ 18% |',
            '| a | 3 | ▏ <1% |',
            '| b | 3 | ▏ <1% |']))
        self.assertEqual(lines.block({'docs': 12}), '\n'.join([
            '<!-- af-pr-lines:v1 -->', '**Lines added: 12**', '', '| Directory | Added | Share |',
            '|:--|--:|:--|', '| docs | 12 | ████████████ 100% |', '<!-- /af-pr-lines -->', '']))
        self.assertEqual(lines.render({'a|b': 1}).splitlines()[-1], '| a\\|b | 1 | ████████████ 100% |')

    def test_the_block_parses_back_in_any_row_order_and_refuses_other_shapes(self):
        body = lines.render({'docs': 10_000, 'tests': 5000, 'crates': 12_345, 'a|b': 3})
        parsed, problem = lines.parse(body.splitlines())
        self.assertIsNone(problem)
        self.assertEqual(parsed, {'total': '27k', 'rows': frozenset([
            ('crates', '12k', '████████████', '45%'), ('docs', '10k', '█████████▊', '37%'),
            ('tests', '5k', '████▉', '18%'), ('a|b', '3', '▏', '<1%')])})
        rows = body.splitlines()
        shuffled = rows[:4] + rows[4:][::-1]
        self.assertEqual(lines.parse(shuffled)[0], parsed, 'rows in any order')
        self.assertEqual(lines.parse(['  ' + line + '  ' for line in rows] + ['', ' '])[0], parsed)
        self.assertEqual(lines.parse(['**Lines added: none**']), ({'total': 'none', 'rows': frozenset()}, None))
        head = rows[:4]
        for bad, why in [
                ([], 'the block is empty'),
                (['Lines added: docs/10k tests/5k'], 'the first line is not the title'),
                (['**Lines added: 27k** (additions)'], 'the first line is not the title'),
                (['**Lines added: 27000**'], 'the first line is not the title'),
                (['**Lines added: none**', '| docs | 3 | ▏ 100% |'], 'is the whole block, but more follows'),
                (['**Lines added: 27k**'], 'the title is not followed by the table'),
                (['**Lines added: 27k**', '| Directory | Added |', '|:--|--:|', '| docs | 3 | ▏ 100% |'],
                 'is not the table header'),
                (['**Lines added: 27k**', '| Directory | Added | Share |', '| docs | 3 | ▏ 100% |',
                  '| tests | 3 | ▏ 100% |'], 'is not followed by its separator'),
                (head + ['| docs | 3 |'], 'not a row of three cells'),
                (head + ['docs | 3 | ▏ 100%'], 'not a row of three cells'),
                (head + ['| docs | 5000 | ▏ 100% |'], 'not a row `| directory | figure | bar share |`'),
                (head + ['| docs | 1.25k | ▏ 100% |'], 'not a row `| directory | figure | bar share |`'),
                (head + ['| docs | 3 | 100% |'], 'not a row `| directory | figure | bar share |`'),
                (head + ['| docs | 3 | ▏ |'], 'not a row `| directory | figure | bar share |`'),
                (head + ['| docs | 3 | ▏ 0% |', '| docs | 4 | ▏ 0% |'], '`docs` has two rows'),
                (head + ['|  | 3 | ▏ 100% |'], 'not a row `| directory | figure | bar share |`')]:
            parsed, problem = lines.parse(bad)
            self.assertIsNone(parsed, bad)
            self.assertIn(why, problem, bad)


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
        self.assertEqual(done.stdout, '\n'.join([
            '<!-- af-pr-lines:v1 -->', '**Lines added: 157**', '', '| Directory | Added | Share |',
            '|:--|--:|:--|', '| crates | 121 | ████████████ 77% |', '| tests | 31 | ███▏ 20% |',
            '| docs | 3 | ▎ 2% |', '| README.md | 2 | ▎ 1% |', '<!-- /af-pr-lines -->', '']))
        done = self.run_tool('--base', 'main', '--json')
        self.assertEqual(json.loads(done.stdout), {'README.md': 2, 'crates': 121, 'tests': 31,
                                                   'docs': 3})
        done = self.run_tool('--base', 'main', '--head', 'main')
        self.assertEqual(done.stdout, '<!-- af-pr-lines:v1 -->\n**Lines added: none**\n'
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
        rows = right.splitlines()
        reordered = '\n'.join(rows[:5] + rows[5:9][::-1] + rows[9:]) + '\n'
        self.assertNotEqual(reordered, right)
        stale = right.replace('| crates | 121 |', '| crates | 120 |')
        body = lambda block: ('## What\n\nX.\n\n## af task report\n\n' + REPORT
                              + '\n## Lines added\n\n' + block + '\n')
        code, said = self.check(body(right), '--diff', 'main', 'feature')
        self.assertEqual(code, 0, said)
        self.assertIn('the figures match the pull request', said)
        code, said = self.check(body(reordered), '--diff', 'main', 'feature')
        self.assertEqual(code, 0, said)
        code, said = self.check(body(stale), '--diff', 'main', 'feature')
        self.assertEqual(code, 1, said)
        self.assertIn('replace it with:\n**Lines added: 157**', said)
        self.assertIn('| crates | 121 | ████████████ 77% |', said)
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
        self.assertIn('replace it with:\n**Lines added: 157**', said)
        event['pull_request']['head']['sha'] = '0' * 40
        code, said = self.check(None, event=event)
        self.assertEqual(code, 1, said)
        self.assertIn('could not count the lines added', said)


if __name__ == '__main__':
    unittest.main()
