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
    def test_rounding_and_zero_denominators(self):
        for n, shown in [(0, '0'), (999, '999'), (1049, '1k'), (1050, '1.1k'),
                         (9950, '10k'), (10500, '11k'), (2500000, '2500k')]:
            self.assertEqual(lines.figure(n), shown)
        for n, total, shown in [(0, 0, '—'), (0, 1, '0%'), (1, 3, '33%'),
                                (2, 3, '67%'), (1, 200, '1%'), (1, 201, '<1%')]:
            self.assertEqual(lines.share(n, total), shown)


class Hierarchy(unittest.TestCase):
    def test_minimal_root_and_crate_collisions_label_only_the_file(self):
        for path in ['crates', 'crates/a']:
            body = lines.render_entries([(3, 0, 'crates/a/src/lib.rs'), (0, 2, path)])
            label = lines.escape(path.split('/')[-1] + ' (file)')
            self.assertIn(label, body)
            self.assertIsNone(lines.parse(body.splitlines())[1])
            self.assertEqual(body.count('| Area |'), 1)

    def test_collision_only_aliases_reserve_literals_and_escape_after_projection(self):
        for name in ['a', ' &nbsp;**雪|\t\u00a0', 'a (file)']:
            entries = [(3, 0, f'crates/{name}/src/lib.rs'), (0, 2, f'crates/{name}'),
                       (1, 1, f'crates/{name} (file)'),
                       (1, 0, f'crates/{name} (file2)/src/lib.rs')]
            data = lines.report(entries)
            rows = data['rows'][0]['children']
            projected = lines.display_names(rows)
            self.assertEqual(projected[name], name + ' (file3)')
            self.assertEqual(projected[name + '/'], name)
            self.assertEqual(projected[name + ' (file)'], name + ' (file)')
            self.assertEqual(len(set(projected.values())), len(rows))
            self.assertEqual(projected, lines.display_names(rows[::-1]))
            body = lines.render(data)
            self.assertIn(lines.escape(name + ' (file3)'), body)
            self.assertIsNone(lines.parse(body.splitlines())[1])
            self.assertEqual(body, lines.render_entries(entries[::-1]))
        self.assertEqual(lines.display_names([{'name': 'a (file)'}]),
                         {'a (file)': 'a (file)'})

    def test_path_classification_including_inline_and_common_source(self):
        for path, kind in {
            'src/lib.rs': 'main/', 'src/common/helpers.rs': 'main/',
            'src/shared.rs': 'main/', 'src/inline.rs': 'main/',
            'src/tests/unit.rs': 'tests/', 'src/tests.rs': 'tests/',
            'src/test_parser.rs': 'tests/', 'src/parser_test.rs': 'tests/',
            'tests/it/a.rs': 'tests/', 'examples/test_demo.rs': 'tests/',
            'tests/data.txt': 'other/', 'examples/demo.rs': 'other/',
            'src/schema.json': 'other/', 'build.rs': 'other/', 'Cargo.toml': 'other/',
            'not-src/a.rs': 'other/'}.items():
            self.assertEqual(lines.code_kind(path.split('/')), kind, path)

    def test_independent_exact_partition_order_and_human_table(self):
        entries = [(3, 2, 'crates/a/src/lib.rs'), (0, 4, 'crates/a/tests/t.rs'),
                   (0, 2, 'crates/b/src/lib.rs'), (1, 1, 'crates/direct'),
                   (0, 8, 'gone/a'), (1, 0, 'README.md')]
        data = lines.report(entries)
        self.assertEqual((data['added'], data['deleted']), (5, 17))
        def reconcile(rows, added, deleted):
            self.assertEqual(sum(r['added'] for r in rows), added)
            self.assertEqual(sum(r['deleted'] for r in rows), deleted)
            for r in rows:
                self.assertTrue(r['added'] or r['deleted'])
                if r['children']:
                    reconcile(r['children'], r['added'], r['deleted'])
        reconcile(data['rows'], 5, 17)
        self.assertEqual(data, lines.report(entries[::-1]))
        body = lines.render(data)
        self.assertEqual(body, '\n'.join([
            '**Lines added: 5**', '', '| Area | Changes | Share of additions |', '|:--|--:|:--|',
            '| **crates** | **+4/−9** | **80%** |',
            '| &nbsp;&nbsp;&nbsp;&nbsp;**a** | **+3/−6** | **75%** |',
            '| &nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;main | +3/−2 | 100% |',
            '| &nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;tests | +0/−4 | 0% |',
            '| &nbsp;&nbsp;&nbsp;&nbsp;direct | +1/−1 | 25% |',
            '| &nbsp;&nbsp;&nbsp;&nbsp;**b** | **+0/−2** | **0%** |',
            '| &nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;main | +0/−2 | — |',
            '| **README.md** | **+1/−0** | **20%** |',
            '| **gone** | **+0/−8** | **0%** |']))
        self.assertIsNone(lines.parse(body.splitlines())[1])
        self.assertEqual(body.count('| Area |'), 1)
        self.assertNotIn('↳', body)
        self.assertNotIn('█', body)
        self.assertNotIn('shared', body)
        self.assertNotIn('| **tests**', body)
        self.assertIn('| **gone** | **+0/−8** | **—** |',
                      lines.render_entries([(0, 8, 'gone/a')]))
        zero = lines.render_entries([(0, 8, 'crates/a/tests/t.rs')])
        self.assertIn('**Lines added: 0**', zero)
        self.assertEqual(zero.count('—'), 3)
        self.assertIsNone(lines.parse(zero.splitlines())[1])

    def test_deletion_siblings_sort_by_deleted_then_raw_identity(self):
        entries = [(0, 2, 'z/a'), (0, 3, 'b/a'), (0, 3, 'a/a'), (1, 0, 'last/a')]
        self.assertEqual([r['name'] for r in lines.report(entries)['rows']],
                         ['last/', 'a/', 'b/', 'z/'])
        self.assertEqual(lines.report(entries), lines.report(entries[::-1]))

    def test_empty_binary_noop_and_pure_rename_have_no_numeric_rows(self):
        for entries in [[], [(None, None, 'binary')], [(0, 0, 'crates/a/src/new.rs')]]:
            self.assertEqual(lines.report(entries),
                             {'schema': 'af/PrLines@4', 'added': 0, 'deleted': 0, 'rows': []})
            self.assertEqual(lines.render_entries(entries), '**Lines added: none**')
        # A crate literally named tests is still ordinary source.
        self.assertEqual(lines.report([(1, 0, 'crates/tests/src/lib.rs')])['rows'][0]
                         ['children'][0]['children'][0]['name'], 'main/')
        self.assertIsNone(lines.parse(lines.render_entries([(1, 0, 'crates')]).splitlines())[1])

    def test_names_cannot_spoof_structure_and_parser_rejects_malformed_rows(self):
        entries = [(1, 0, 'crates/ &nbsp;**↳雪|\t/src/lib.rs'),
                   (1, 1, 'crates/direct'), (0, 2, 'gone/a')]
        body = lines.render_entries(entries)
        self.assertIn('&#32;&#38;nbsp;&#42;&#42;&#8627;雪&#124;&#9;', body)
        self.assertNotIn('\t', body)
        self.assertIsNone(lines.parse(body.splitlines())[1])
        nbsp = lines.render_entries([(1, 0, 'crates/\u00a0\u00a0\u00a0\u00a0/src/lib.rs'),
                                     (1, 0, 'crates/&nbsp;&nbsp;&nbsp;&nbsp;/src/lib.rs')])
        self.assertIn('**&#34;&#160;&#160;&#160;&#160;&#34;**', nbsp)
        self.assertIn('**&#38;nbsp;&#38;nbsp;&#38;nbsp;&#38;nbsp;**', nbsp)
        self.assertIsNone(lines.parse(nbsp.splitlines())[1])
        valid = lines.render_entries([(1, 1, 'crates/a/src/lib.rs'), (0, 1, 'crates/a/tests/t.rs')])
        for before, after in [('Area', 'Directory'), ('+1/−2', '+0/−0'),
                              ('**crates**', 'crates'), ('**+1/−2**', '+1/−2'),
                              ('**a**', 'a'), ('main', '**main**'), ('tests', 'shared'),
                              ('&nbsp;&nbsp;&nbsp;&nbsp;**a**', '&nbsp;**a**'),
                              ('100%', '101%'), ('100%', '—'), ('0%', '<1%'),
                              ('**a**', '**a/**'), ('+1/−1', '+01/−1')]:
            self.assertIn(before, valid)
            self.assertIsNone(lines.parse(valid.replace(before, after).splitlines())[0], after)
        rows = valid.splitlines()
        for bad in [rows[:4]+rows[5:], rows[:6], rows+rows[4:5], rows[:5]+rows[6:]]:
            self.assertIsNone(lines.parse(bad)[0], bad)
        for bad in [[], ['**Lines added: 1**'], ['**Lines added: none**', 'x']]:
            self.assertIsNone(lines.parse(bad)[0])

    def test_json_schema_exact_current_counts_and_old_shapes_fail_closed(self):
        schema_path = SCRIPTS.parent / 'schemas/pr-lines-v4.json'
        spec = importlib.util.spec_from_file_location('schema_check', SCRIPTS / 'backlog-check-triage.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        validator = module.Validator(schema_path)
        schema_id = json.loads(schema_path.read_text())['$id']
        for entries in [[], [(0, 3, 'crates/a/tests/t.rs')], [(1, 0, 'crates/direct')],
                        [(1, 2, 'crates/a/src/lib.rs'), (0, 5, 'gone/a')]]:
            data = lines.report(entries)
            self.assertEqual(validator.errors(data, schema_id), [])
        for bad in [{**data, 'schema': 'af/PrLines@3'}, {**data, 'total': 1},
                    {**data, 'deleted': -1}, {**data, 'rows': [
                        {'name': 'docs/', 'added': 0, 'deleted': 0, 'children': []}]}]:
            self.assertTrue(validator.errors(bad, schema_id), bad)
        self.assertFalse((SCRIPTS.parent / 'schemas/pr-lines-v3.json').exists())
        tree = lines.report([(1, 2, 'crates/a/src/lib.rs')])
        for field, value in [('name', 'shared/'), ('deleted', -1), ('unknown', True),
                             ('children', [{'name': 'main/', 'added': 1,
                                            'deleted': 0, 'children': []}])]:
            bad = json.loads(json.dumps(tree))
            bad['rows'][0]['children'][0]['children'][0][field] = value
            self.assertTrue(validator.errors(bad, schema_id), bad)


class Repository(unittest.TestCase):
    """A base commit and a branch over it that adds, moves, deletes and binary-changes files."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name) / 'repo'
        self.repo.mkdir()
        git(self.repo, 'init', '-q', '-b', 'main')
        write(self.repo, 'unchanged/old.txt', 'pure rename\n')
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
        git(self.repo, 'mv', 'unchanged/old.txt', 'unchanged/new.txt')
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
        self.assertEqual(entries['unchanged/new.txt'], (0, 0))

    def test_real_git_file_directory_transitions_keep_identities_and_exact_sums(self):
        # Reuse this repository; each range covers just the conversion's two commits.
        for path, nested in [('crates', 'crates/a/src/lib.rs'),
                             ('crates/a', 'crates/a/src/lib.rs')]:
            git(self.repo, 'rm', '-qr', 'crates')
            write(self.repo, path, 'old one\nold two\n')
            write(self.repo, path + ' (file)', 'literal\n')
            git(self.repo, 'add', '-A')
            git(self.repo, 'commit', '-qm', 'file baseline')
            for reverse in [False, True]:
                base = git(self.repo, 'rev-parse', 'HEAD').strip()
                git(self.repo, 'rm', '-qr', path)
                write(self.repo, path if reverse else nested,
                      'old one\nold two\n' if reverse else 'new one\nnew two\nnew three\n')
                # A changed literal alias must reserve its name in the displayed sibling set.
                write(self.repo, path + ' (file)', 'literal\nextra\n' if not reverse else 'literal\n')
                git(self.repo, 'add', '-A')
                git(self.repo, 'commit', '-qm', 'type transition')
                data = json.loads(self.run_tool('--base', base, '--json').stdout)
                self.assertEqual((data['added'], data['deleted']),
                                 (2, 4) if reverse else (4, 2))

                def reconcile(rows, added, deleted):
                    self.assertEqual(sum(r['added'] for r in rows), added)
                    self.assertEqual(sum(r['deleted'] for r in rows), deleted)
                    for row in rows:
                        if row['children']:
                            reconcile(row['children'], row['added'], row['deleted'])
                reconcile(data['rows'], data['added'], data['deleted'])
                siblings = data['rows'] if path == 'crates' else data['rows'][0]['children']
                identities = {r['name']: (r['added'], r['deleted']) for r in siblings}
                name = path.split('/')[-1]
                self.assertEqual(identities[name], (2, 0) if reverse else (0, 2))
                self.assertEqual(identities[name + '/'], (0, 3) if reverse else (3, 0))
                self.assertEqual(identities[name + ' (file)'], (0, 1) if reverse else (1, 0))
                body = self.run_tool('--base', base).stdout
                self.assertIn(lines.escape(name + ' (file2)'), body)
                self.assertIsNone(lines.parse(body.splitlines()[1:-1])[1])
                code, said = self.check(REPORT + '\n' + body, '--diff', base, 'HEAD')
                self.assertEqual(code, 0, said)

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
        self.assertEqual(splits[name + '/'], {'main/': 2, 'tests/': 1})
        self.assertEqual(splits['af/'], {'main/': 120, 'tests/': 31})
        rendered = self.run_tool('--base', 'main').stdout
        self.assertIn('&#8627;&#32;雪a&#124;b&#38;&#60;x&#62;&#96;&#42;&#95;&#91;q&#93;&#92;&#9;&#10;', rendered)
        self.assertIsNone(lines.parse(rendered.splitlines()[1:-1])[1])
        code, said = self.check(REPORT + '\n' + rendered, '--diff', 'main', 'HEAD')
        self.assertEqual(code, 0, said)

    def test_the_tool_prints_the_block_and_json(self):
        done = self.run_tool('--base', 'main', '--head', 'feature')
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(done.stdout, '\n'.join([
            '<!-- af-pr-lines:v1 -->', '**Lines added: 157**', '', '| Area | Changes | Share of additions |',
            '|:--|--:|:--|', '| **crates** | **+151/−1** | **96%** |',
            '| &nbsp;&nbsp;&nbsp;&nbsp;**af** | **+151/−1** | **100%** |',
            '| &nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;main | +121/−1 | 80% |',
            '| &nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;tests | +30/−0 | 20% |',
            '| **docs** | **+3/−3** | **2%** |', '| **README.md** | **+2/−0** | **1%** |', '| **scripts** | **+1/−0** | **1%** |',
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
        stale = right.replace('main | +121/−1', 'main | +120/−1')
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
        self.assertIn('| **crates** | **+151/−1** | **96%** |', said)
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
