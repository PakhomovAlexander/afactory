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


def flat(totals):
    return lines.report([(n, 0, name) for name, n in totals.items()])


class Figures(unittest.TestCase):
    def test_a_count_is_exact_then_rounded(self):
        for n, shown in [(0, '0'), (7, '7'), (999, '999'), (1000, '1k'), (1049, '1k'),
                         (1050, '1.1k'), (4980, '5k'), (7887, '7.9k'), (9949, '9.9k'),
                         (9950, '10k'), (10_000, '10k'), (10_499, '10k'), (10_500, '11k'),
                         (23_286, '23k'), (123_456, '123k'), (2_500_000, '2500k')]:
            self.assertEqual(lines.figure(n), shown, n)

    def test_a_bar_is_the_count_against_the_largest_in_eighths_and_a_share_a_percent(self):
        for n, largest, expected in [(12_345, 12_345, '████████████'),
                                     (6172, 12_345, '██████'), (1, 12_345, '▏'),
                                     (10_000, 12_345, '█████████▊')]:
            self.assertEqual(lines.bar(n, largest), expected)
        for n, total, expected in [(1, 3, '33%'), (2, 3, '67%'), (1, 200, '1%'),
                                   (1, 201, '<1%'), (12_345, 12_345, '100%')]:
            self.assertEqual(lines.share(n, total), expected)

    def test_single_table_largest_first_or_none_and_safe_names(self):
        self.assertEqual(lines.render(flat({})), '**Lines added: none**')
        self.assertEqual(lines.render(flat({'docs': 10_000, 'tests': 5000, 'crates': 12_345,
                                           'b': 3, 'a': 3})), '\n'.join([
            '**Lines added: 27k**', '', '| Directory | Added | Share |', '|:--|--:|:--|',
            '| crates | 12k | ████████████ 45% |', '| docs | 10k | █████████▊ 37% |',
            '| tests | 5k | ████▉ 18% |', '| a | 3 | ▏ <1% |', '| b | 3 | ▏ <1% |']))
        self.assertEqual(lines.block(flat({'docs': 12})), '\n'.join([
            '<!-- af-pr-lines:v1 -->', '**Lines added: 12**', '', '| Directory | Added | Share |',
            '|:--|--:|:--|', '| docs | 12 | ████████████ 100% |', '<!-- /af-pr-lines -->', '']))
        self.assertIn('| a&#124;b |', lines.render(flat({'a|b': 1})))

    def test_shape_parser_rejects_malformed_tables_without_summing_rounded_figures(self):
        body = lines.render(flat({'docs': 10_000, 'tests': 5000, 'crates': 12_345, 'a|b': 3}))
        parsed, problem = lines.parse(body.splitlines())
        self.assertIsNone(problem)
        self.assertEqual(parsed['total'], '27k')
        self.assertIn((('a&#124;b',), '3', '▏', '<1%'), parsed['rows'])
        rows = body.splitlines()
        self.assertEqual(lines.parse(rows[:4] + rows[4:][::-1])[0], parsed)
        self.assertEqual(lines.parse(['  ' + line + '  ' for line in rows] + ['', ' '])[0], parsed)
        self.assertEqual(lines.parse(['**Lines added: none**']),
                         ({'total': 'none', 'rows': frozenset()}, None))
        head = rows[:4]
        for bad in [[], ['Lines added: docs/10k tests/5k'], ['**Lines added: 27k** (additions)'],
                    ['**Lines added: 27000**'], ['**Lines added: none**', '| x |'],
                    ['**Lines added: 27k**'],
                    ['**Lines added: 27k**', '| Directory | Added |', '|:--|--:|', '| x |'],
                    ['**Lines added: 27k**', rows[2], '| no separator |', rows[4]]]:
            self.assertIsNone(lines.parse(bad)[0], bad)
        for row in ['| docs | 3 |', 'docs | 3 | ▏ 100%', '| docs | 5000 | ▏ 100% |',
                    '| docs | 1.25k | ▏ 100% |', '| docs | 3 | 100% |', '| docs | 3 | ▏ |',
                    '| | 3 | ▏ 100% |', '| docs | 0 | ▏ 100% |', '| docs | ٣ | ▏ 100% |']:
            self.assertIsNone(lines.parse(head + [row])[0], row)
        self.assertIn('two rows', lines.parse(head + [rows[4], rows[4]])[1])


class Hierarchy(unittest.TestCase):
    def test_rust_test_paths_take_priority_no_shared_category(self):
        paths = {
            'src/lib.rs': 'main/', 'src/shared.rs': 'main/', 'src/common/helpers.rs': 'main/',
            'src/inline.rs': 'main/', 'src/tests/unit.rs': 'test/', 'src/tests.rs': 'test/',
            'src/test_parser.rs': 'test/', 'src/parser_test.rs': 'test/',
            'tests/it/a.rs': 'test/', 'examples/test_example.rs': 'test/',
            'examples/demo.rs': 'other/', 'src/schema.json': 'other/',
            'tests/input.txt': 'other/', 'Cargo.toml': 'other/', 'build.rs': 'other/',
            'not-src/file.rs': 'other/', 'test_utils.py': 'other/', 'tests': 'other/',
        }
        for path, kind in paths.items():
            self.assertEqual(lines.code_kind(path.split('/')), kind, path)

    def test_crate_named_tests_does_not_change_source_classification(self):
        data = lines.report([(3, 0, 'crates/tests/src/lib.rs')])
        self.assertEqual(data['rows'][0]['children'][0]['children'],
                         [{'name': 'main/', 'added': 3, 'children': []}])

    def test_exact_partition_one_table_parent_shares_sibling_bars_and_order(self):
        entries = [(10, 0, 'crates/b/src/lib.rs'), (5, 0, 'crates/a/src/tests.rs'),
                   (2, 0, 'crates/a/tests/data.txt'), (3, 0, 'crates/a/src/lib.rs'),
                   (4, 0, 'docs/guide.md'), (3, 0, 'crates/README.md'), (2, 0, 'Cargo.lock'),
                   (1, 0, 'scripts/performance/tests/test_a.py')]
        data = lines.report(entries)
        self.assertEqual(data['schema'], 'af/PrLines@3')
        self.assertEqual(data['total'], 30)
        def reconcile(rows, parent):
            self.assertEqual(sum(row['added'] for row in rows), parent)
            for row in rows:
                self.assertGreater(row['added'], 0)
                if row['children']:
                    reconcile(row['children'], row['added'])
        reconcile(data['rows'], 30)
        self.assertEqual(data, lines.report(list(reversed(entries))))
        rendered = lines.render(data)
        self.assertEqual(rendered, '\n'.join([
            '**Lines added: 30**', '', '| Directory | Added | Share |', '|:--|--:|:--|',
            '| crates/ | 23 | ████████████ 77% |',
            '| ↳ a/ | 10 | ████████████ 43% |',
            '| ↳ ↳ test/ | 5 | ████████████ 50% |',
            '| ↳ ↳ main/ | 3 | ███████▎ 30% |',
            '| ↳ ↳ other/ | 2 | ████▊ 20% |',
            '| ↳ b/ | 10 | ████████████ 43% |',
            '| ↳ ↳ main/ | 10 | ████████████ 100% |',
            '| ↳ README.md | 3 | ███▋ 13% |',
            '| docs/ | 4 | ██▏ 13% |',
            '| Cargo.lock | 2 | █ 7% |',
            '| scripts/ | 1 | ▌ 3% |']))
        self.assertEqual(rendered.count('| Directory | Added | Share |'), 1)
        self.assertNotIn('Crate additions', rendered)
        self.assertNotIn('| tests', rendered)
        self.assertIsNone(lines.parse(rendered.splitlines())[1])

    def test_direct_crates_file_root_file_empty_binary_and_deletions(self):
        for path in ['crates', 'crates/README.md', 'README.md', 'docs/tests/a.rs']:
            data = lines.report([(3, 0, path), (None, None, 'crates/b/src/b.rs'),
                                 (0, 4, 'crates/a/src/lib.rs')])
            self.assertEqual(data['total'], 3)
            expected = path.split('/')[0] + ('/' if '/' in path else '')
            self.assertEqual(data['rows'][0]['name'], expected)
            self.assertIsNone(lines.parse(lines.render(data).splitlines())[1])
        data = lines.report([(2, 0, 'crates/direct.rs')])
        self.assertEqual(data['rows'][0]['children'],
                         [{'name': 'direct.rs', 'added': 2, 'children': []}])
        for entries in [[], [(0, 4, 'a')], [(None, None, 'crates/a/src/lib.rs')]]:
            self.assertEqual(lines.report(entries), {'schema': 'af/PrLines@3', 'total': 0, 'rows': []})
            self.assertEqual(lines.render_entries(entries), '**Lines added: none**')

    def test_tree_like_unicode_and_markdown_names_are_data(self):
        entries = [(1, 0, '↳ main/one'), (2, 0, 'crates/↳ main/src/lib.rs'),
                   (3, 0, 'crates/↳ file'), (4, 0, 'crates/雪&<x>`*_[q]~|/tests.rs')]
        rendered = lines.render_entries(entries)
        self.assertIn('&#8627;&#32;main/', rendered)
        self.assertIn('↳ &#8627;&#32;file', rendered)
        self.assertIn('雪&#38;&#60;x&#62;&#96;&#42;&#95;&#91;q&#93;&#126;&#124;/', rendered)
        parsed, problem = lines.parse(rendered.splitlines())
        self.assertIsNone(problem)
        self.assertEqual(len(parsed['rows']), 7)

    def test_missing_parents_duplicates_empty_branches_and_invalid_hierarchy_fail_closed(self):
        head = ['**Lines added: 3**', '| Directory | Added | Share |', '|:--|--:|:--|']
        row = lambda name: f'| {name} | 1 | █ 100% |'
        for names in [['crates/'], ['crates/', '↳ a/'], ['↳ a/'],
                      ['crates/', '↳ ↳ main/'], ['crates/', '↳ file', '↳ ↳ main/'],
                      ['docs/', '↳ a/', '↳ ↳ main/'],
                      ['crates/', '↳ a/', '↳ ↳ shared/'],
                      ['crates/', '↳ a/', '↳ ↳ ↳ main/'],
                      ['crates/', '↳ a/', '↳ ↳ main/', '↳ ↳ main/'],
                      ['crates/', '↳ a/', '↳ ↳ main/', '↳ a/', '↳ ↳ test/'],
                      ['docs/', 'docs/'], ['↳fake'], ['crates//'], ['/']]:
            self.assertIsNone(lines.parse(head + [row(name) for name in names])[0], names)
        valid = head + [row(name) for name in ['crates/', '↳ a/', '↳ ↳ main/']]
        for suffix in ['**Crate additions**', '| Crate | Main code | Tests | Other | Total | Share |']:
            self.assertIsNone(lines.parse(valid + [suffix])[0])

    def test_json_schema_matches_current_exact_tree_and_refuses_previous_shape(self):
        schema_path = SCRIPTS.parent / 'schemas/pr-lines-v3.json'
        spec = importlib.util.spec_from_file_location('schema_check',
                                                     SCRIPTS / 'backlog-check-triage.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        validator = module.Validator(schema_path)
        schema_id = json.loads(schema_path.read_text())['$id']
        for entries in ([], [(3, 0, 'crates/a/src/lib.rs')], [(2, 0, 'docs/a.md')],
                        [(1, 0, 'crates/file')], [(2, 0, 'crates/↳ 雪\n/src/lib.rs')]):
            data = lines.report(entries)
            self.assertEqual(validator.errors(data, schema_id), [])
        for bad in ({'docs': 2}, {'schema': 'af/PrLines@2', 'directories': {}, 'crates': {}},
                    {**data, 'schema': 'af/PrLines@2'}, {**data, 'total': -1},
                    {**data, 'rows': [{'name': 'crates/', 'added': 1, 'children': []}]},
                    {**data, 'rows': [{'name': 'docs/', 'added': 0, 'children': []}]},
                    {**data, 'unknown': 0}):
            self.assertTrue(validator.errors(bad, schema_id), bad)
        tree = lines.report([(3, 0, 'crates/a/src/lib.rs')])
        for key, value in [('name', 'shared/'), ('added', -1), ('extra', True),
                           ('children', [{'name': 'main/', 'added': 1, 'children': []}])]:
            bad = json.loads(json.dumps(tree))
            bad['rows'][0]['children'][0]['children'][0][key] = value
            self.assertTrue(validator.errors(bad, schema_id), bad)



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
        self.assertEqual({row['name']: row['added'] for row in lines.report(lines.numstat(self.repo, 'main', 'feature'))['rows']}, {
            'README.md': 2, 'crates/': 151, 'scripts/': 1, 'docs/': 3})
        self.assertEqual({row['name']: row['added'] for row in lines.report(lines.numstat(self.repo, self.base, self.head))['rows']}, {
            'README.md': 2, 'crates/': 151, 'scripts/': 1, 'docs/': 3})
        entries = {path: (added, deleted) for added, deleted, path
                   in lines.numstat(self.repo, 'main', 'feature')}
        self.assertEqual(entries['fixtures/image.png'], (None, None), 'binary')
        self.assertEqual(entries['crates/af/src/moved.rs'], (1, 1), 'the rename, under its new path')
        self.assertNotIn('crates/af/src/old.rs', entries)
        self.assertEqual(entries['docs/gone.md'], (0, 3))

    def test_crate_rename_binary_inline_tests_and_escaped_names_through_git(self):
        name = '↳ 雪a|b&<x>`*_[q]\\\t\n'
        moved = f'crates/{name}/tests/moved.rs'
        git(self.repo, 'mv', 'crates/af/src/moved.rs', 'crates/af/tests/moved.rs')
        write(self.repo, moved, 'test line\n')
        write(self.repo, f'crates/{name}/src/lib.rs',
              '#[cfg(test)]\nmod tests {}\n')
        write(self.repo, f'crates/{name}/tests/image.bin', b'\x00binary')
        git(self.repo, 'add', '-A')
        git(self.repo, 'commit', '-qm', 'crate edge cases')
        data = json.loads(self.run_tool('--base', 'main', '--json').stdout)
        crates = {row['name']: row for row in data['rows']}['crates/']['children']
        splits = {row['name']: {leaf['name']: leaf['added'] for leaf in row['children']}
                  for row in crates}
        self.assertEqual(splits[name + '/'], {'main/': 2, 'test/': 1})
        self.assertEqual(splits['af/'], {'main/': 120, 'test/': 31})
        rendered = self.run_tool('--base', 'main').stdout
        self.assertIn('&#8627;&#32;雪a&#124;b&#38;&#60;x&#62;&#96;&#42;&#95;&#91;q&#93;&#92;&#9;&#10;', rendered)
        self.assertIsNone(lines.parse(rendered.splitlines()[1:-1])[1])
        code, said = self.check(REPORT + '\n' + rendered, '--diff', 'main', 'HEAD')
        self.assertEqual(code, 0, said)

    def test_the_tool_prints_the_block_and_json(self):
        done = self.run_tool('--base', 'main', '--head', 'feature')
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(done.stdout, '\n'.join([
            '<!-- af-pr-lines:v1 -->', '**Lines added: 157**', '', '| Directory | Added | Share |',
            '|:--|--:|:--|', '| crates/ | 151 | ████████████ 96% |',
            '| ↳ af/ | 151 | ████████████ 100% |',
            '| ↳ ↳ main/ | 121 | ████████████ 80% |',
            '| ↳ ↳ test/ | 30 | ███ 20% |',
            '| docs/ | 3 | ▎ 2% |', '| README.md | 2 | ▏ 1% |', '| scripts/ | 1 | ▏ 1% |',
            '<!-- /af-pr-lines -->', '']))
        done = self.run_tool('--base', 'main', '--json')
        self.assertEqual(json.loads(done.stdout),
                         lines.report(lines.numstat(self.repo, 'main', 'HEAD')))
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
        right = lines.block(lines.report(lines.numstat(self.repo, 'main', 'feature')))
        rows = right.splitlines()
        # Reorder only siblings, keeping each complete subtree contiguous.
        reordered = '\n'.join(rows[:9] + rows[9:12][::-1] + rows[12:]) + '\n'
        self.assertNotEqual(reordered, right)
        stale = right.replace('| ↳ ↳ main/ | 121 |', '| ↳ ↳ main/ | 120 |')
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
        self.assertIn('| crates/ | 151 | ████████████ 96% |', said)
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
