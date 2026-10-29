#!/usr/bin/env python3
"""Exercise scripts/test-quarantine.py scan on test-local task lists, inventories and quarantine
files with an explicit today; never this checkout's own quarantine file, the clock, af, cargo or
a real af Store."""
import copy
import datetime as dt
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / 'scripts/test-quarantine.py'
SPEC = importlib.util.spec_from_file_location('test_quarantine', SCRIPT)
tq = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tq)
tc = tq.tc

TODAY = '2026-10-29'
# The window is the 7 days ending at the end of today (UTC): [10-23 00:00, 10-30 00:00).
END = int(dt.datetime(2026, 10, 30, tzinfo=dt.timezone.utc).timestamp() * 1000)
START = END - 7 * 86_400_000
NOON = END - 12 * 3_600_000
CAPTURE = 'review-source-git::it capture::a_settled_worktree_is_admitted_on_retry'
HEAVY = 'af::it task_heavy::loaded'
IGNORED = 'af::it perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample'
FEATURE = 'review-core perf_trace::perf_case'
SHARED = ('review-core shared::same_name', 'review-graph shared::same_name')
# What crates/af/tests/it/task_gate.rs prints, and af 0.13.0 recorded as flaky (#436).
FRAGMENTS = ('gate-store-regression', 'fails_when_crowded', 'af::it', 'many_0')
MANY = [f'review-graph many::case_{i:02}' for i in range(25)]


def inventory():
    suites = {}
    for identity in (CAPTURE, 'review-source-git::it capture::other', HEAVY, IGNORED, FEATURE,
                     *SHARED, *MANY):
        binary, name = identity.split(' ')
        suites.setdefault(binary, {'testcases': {}})['testcases'][name] = {
            'ignored': identity == IGNORED, 'filter-match': {'status': 'matches'}}
    return {'rust-suites': suites}


def row(task_id, updated, flaky, gate=True):
    """A list row shaped like af's `af/task-list-entry@4`."""
    value = {'schema': 'af/task-list-entry@4', 'task_id': task_id, 'kind': 'af/implement@1',
             'phase': {'kind': 'finished'}, 'state': {'phase': 'finished', 'outcome': 'verified'},
             'chargeable_tokens': '0', 'derived_snapshot_id': None, 'delivery': None, 'round': 1,
             'updated_unix_ms': updated}
    if gate:
        value['gate'] = {'status': 'passed', 'failed_tests': [], 'flaky': list(flaky)}
    return value


def listing(*rows, schema='af/task-list@4'):
    return {'schema': schema, 'tasks': list(rows)}


def held(identity, since='2026-10-25', expires='2026-11-08'):
    return {'state': 'quarantined', 'since': since, 'expires': expires, 'evidence': ['T_OLD']}


class Scan(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name)
        self.inventory = self.dir / 'nextest-list.json'
        self.inventory.write_text(json.dumps(inventory()))
        self.file = self.dir / 'test-quarantine.json'
        tc.write(self.file, {'schema': 'af.test-quarantine/1', 'tests': {}})
        self.lists = 0

    def documents(self, *documents):
        paths = []
        for document in documents:
            self.lists += 1
            path = self.dir / f'list-{self.lists}.json'
            path.write_text(json.dumps(document))
            paths.append(path)
        return paths

    def scan(self, *documents, write=False, json_out=True, today=TODAY, env=None, extra=()):
        arguments = [arg for path in self.documents(*documents) for arg in ('--list', str(path))]
        command = [sys.executable, '-I', str(SCRIPT), 'scan', *arguments, *extra,
                   '--inventory', str(self.inventory), '--quarantine', str(self.file)]
        command += ['--today', today] if today else []
        command += ['--write'] if write else []
        command += ['--json'] if json_out else []
        environment = {k: v for k, v in os.environ.items() if not k.startswith('AF_')}
        environment.update(env or {})
        return subprocess.run(command, capture_output=True, text=True, env=environment)

    def facts(self, *documents, **options):
        result = self.scan(*documents, **options)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def proposed(self, facts):
        return {p['identity']: p['entry']['evidence'] for p in facts['proposed']}

    def assertUnchanged(self, before):
        self.assertEqual(self.file.read_bytes(), before)

    def test_two_distinct_tasks_within_the_window_propose_an_entry_and_change_nothing(self):
        before = self.file.read_bytes()
        files = set(self.dir.iterdir())
        facts = self.facts(listing(row('T_B', NOON, [HEAVY]), row('T_A', START, [HEAVY])))
        self.assertEqual(facts['schema'], 'af.test-quarantine-scan/1')
        self.assertEqual(facts['proposed'], [{'identity': HEAVY, 'tasks': 2, 'entry': {
            'state': 'quarantined', 'since': TODAY, 'expires': '2026-11-12',
            'evidence': ['T_A', 'T_B']}}])
        self.assertFalse(facts['written'])
        self.assertUnchanged(before)
        self.assertEqual(set(self.dir.iterdir()) - files, {self.dir / 'list-1.json'})
        text = self.scan(listing(row('T_B', NOON, [HEAVY]), row('T_A', NOON, [HEAVY])),
                         json_out=False)
        self.assertEqual(text.returncode, 0, text.stderr)
        self.assertIn(f'would add 1 quarantine entries (since {TODAY}):\n  {HEAVY}: flaky in 2 '
                      'Tasks, expires 2026-11-12, evidence T_A, T_B\n', text.stdout)
        self.assertIn('nothing written; rerun with --write', text.stdout)
        self.assertUnchanged(before)

    def test_one_task_or_the_same_task_twice_proposes_nothing(self):
        one = self.facts(listing(row('T_A', NOON, [HEAVY])))
        self.assertEqual(one['proposed'], [])
        # The same Task in one list, and in two lists (two Stores, or one listed twice).
        same = self.facts(listing(row('T_A', NOON, [HEAVY]), row('T_A', START, [HEAVY])))
        self.assertEqual(same['proposed'], [])
        twice = self.facts(listing(row('T_A', NOON, [HEAVY])), listing(row('T_A', NOON, [HEAVY])))
        self.assertEqual(twice['proposed'], [])
        # A row without a gate contributes nothing.
        gateless = self.facts(listing(row('T_A', NOON, [HEAVY]), row('T_B', NOON, [], gate=False)))
        self.assertEqual(gateless['proposed'], [])
        text = self.scan(listing(row('T_A', NOON, [HEAVY])), json_out=False)
        self.assertEqual(text.returncode, 0, text.stderr)
        self.assertEqual(text.stdout, 'no identity was flaky in 2 distinct Tasks within the 7 days '
                                      f'ending {TODAY}\n')

    def test_a_second_task_outside_the_seven_days_proposes_nothing(self):
        for outside in (START - 1, END):
            with self.subTest(outside=outside):
                facts = self.facts(listing(row('T_A', NOON, [HEAVY]),
                                           row('T_B', outside, [HEAVY, 'many_0'])))
                self.assertEqual(facts['proposed'], [])
                # A row outside the window adds no rejected name either.
                self.assertEqual(facts['rejected'], [])
        # The window's first and last millisecond are inside it.
        facts = self.facts(listing(row('T_A', START, [HEAVY]), row('T_B', END - 1, [HEAVY])))
        self.assertEqual(self.proposed(facts), {HEAVY: ['T_A', 'T_B']})
        self.assertEqual(facts['window'], {'start_unix_ms': START, 'end_unix_ms': END})

    def test_an_af_0_13_row_admits_only_its_real_bare_name(self):
        # 0.13.0's text fallback dropped the binary and read the fixture lines as flaky tests.
        flaky = [*FRAGMENTS[:2], 'capture::a_settled_worktree_is_admitted_on_retry']
        facts = self.facts(listing(row('T_A', NOON, flaky), row('T_B', NOON, [*FRAGMENTS, flaky[2]]),
                                   schema='af/task-list@3'))
        self.assertEqual(self.proposed(facts), {CAPTURE: ['T_A', 'T_B']})
        self.assertEqual(facts['rejected'], [{'name': name, 'reason': 'not a test'}
                                             for name in sorted(FRAGMENTS)])

    def test_rejected_names_are_listed_with_their_reason(self):
        facts = self.facts(listing(row('T_A', NOON, ['shared::same_name', IGNORED, FEATURE,
                                                     'perf_trace::perf_case', 'absent::test']),
                                   row('T_B', NOON, [IGNORED, FEATURE, 'shared::same_name'])))
        self.assertEqual(facts['proposed'], [])
        self.assertEqual(facts['rejected'], sorted([
            {'name': 'shared::same_name', 'reason': 'ambiguous: ' + ', '.join(SHARED)},
            {'name': IGNORED, 'reason': 'not a test'}, {'name': FEATURE, 'reason': 'not a test'},
            {'name': 'perf_trace::perf_case', 'reason': 'not a test'},
            {'name': 'absent::test', 'reason': 'not a test'}], key=lambda item: item['name']))
        text = self.scan(listing(row('T_A', NOON, ['shared::same_name'])), json_out=False)
        self.assertIn('rejected shared::same_name: ambiguous: ' + ', '.join(SHARED), text.stdout)

    def test_write_adds_entries_keeps_existing_ones_and_passes_the_audit(self):
        existing = {MANY[0]: held(MANY[0])}
        tc.write(self.file, {'schema': 'af.test-quarantine/1', 'tests': existing})
        original = self.file.read_text()
        start = original.index(f'    "{MANY[0]}": {{')
        block = original[start:original.index('\n    }', start) + 6]
        twelve = [row(f'T_{i:02}', NOON, [HEAVY, MANY[0]]) for i in reversed(range(12))]
        facts = self.facts(listing(*twelve, row('T_X', NOON, [CAPTURE]), row('T_Y', NOON, [CAPTURE])),
                           write=True)
        self.assertTrue(facts['written'])
        self.assertEqual(facts['held'], [MANY[0]])
        value = tc.read(self.file)
        self.assertEqual(value['tests'], {
            MANY[0]: existing[MANY[0]],
            HEAVY: {'state': 'quarantined', 'since': TODAY, 'expires': '2026-11-12',
                    'evidence': [f'T_{i:02}' for i in range(10)]},
            CAPTURE: {'state': 'quarantined', 'since': TODAY, 'expires': '2026-11-12',
                      'evidence': ['T_X', 'T_Y']}})
        # The shipped file's format, the existing entry's bytes kept.
        self.assertEqual(self.file.read_text(), json.dumps(value, indent=2, sort_keys=True) + '\n')
        self.assertIn(block, self.file.read_text())
        self.assertEqual([p['identity'] for p in facts['proposed']], [HEAVY, CAPTURE])
        self.assertEqual(facts['proposed'][0]['tasks'], 12)
        # The written file passes scripts/test-classes.py audit and its partition, same today.
        with mock.patch.object(tc, 'QUARANTINE', self.file), \
                mock.patch.dict(os.environ, {'AF_TEST_QUARANTINE_TODAY': TODAY}):
            tc.audit_source()
            classes = {e['binary'] + ' ' + e['test']: e['class'] for e in tc.partition(inventory())}
        self.assertEqual({i for i, c in classes.items() if c == 'quarantine'}, {MANY[0], HEAVY, CAPTURE})

    def test_write_stops_at_twenty_entries_and_names_the_overflow(self):
        tc.write(self.file, {'schema': 'af.test-quarantine/1', 'tests': {HEAVY: held(HEAVY)}})
        rows = [row(f'T_{i}', NOON, MANY[:21]) for i in range(2)]
        rows.append(row('T_3', NOON, MANY[20:]))  # MANY[20] in 3 Tasks ranks first.
        facts = self.facts(listing(*rows), write=True)
        added = [p['identity'] for p in facts['proposed']]
        self.assertEqual(added, [MANY[20], *MANY[:18]])
        self.assertEqual(facts['over_cap'], MANY[18:20])
        self.assertEqual(len(tc.read(self.file)['tests']), 20)
        text = self.scan(listing(*rows), json_out=False)
        self.assertIn('over the 20-entry cap, not added: ' + ', '.join(MANY[18:20]), text.stdout)
        # A full file adds nothing and names every proposal as over the cap.
        full = self.facts(listing(*rows), write=True)
        self.assertEqual((full['proposed'], full['over_cap'], full['written']), ([], MANY[18:20], False))

    def test_a_guard_refusal_writes_nothing(self):
        cases = [
            # A row's Task ID that is not an af Task ID cannot be evidence.
            ({}, [row('T.A', NOON, [HEAVY]), row('T_B', NOON, [HEAVY])], 'evidence is not'),
            # The result fails the partition-time guard: a held identity absent from the inventory.
            ({'af::it absent::test': held('af::it absent::test')},
             [row('T_A', NOON, [HEAVY]), row('T_B', NOON, [HEAVY])], 'not in the test inventory'),
            # The current file already fails a guard.
            ({CAPTURE: held(CAPTURE, '2026-10-10', '2026-10-20')}, [row('T_A', NOON, [HEAVY])],
             'expired on 2026-10-20'),
        ]
        for tests, rows, needle in cases:
            with self.subTest(needle=needle):
                tc.write(self.file, {'schema': 'af.test-quarantine/1', 'tests': tests})
                before = self.file.read_bytes()
                result = self.scan(listing(*rows), write=True)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn(needle, result.stderr)
                self.assertEqual(result.stdout, '')
                self.assertUnchanged(before)

    def test_both_list_schemas_are_accepted_and_an_unknown_one_is_refused(self):
        for schema in ('af/task-list@3', 'af/task-list@4'):
            with self.subTest(schema=schema):
                facts = self.facts(listing(row('T_A', NOON, [HEAVY]), row('T_B', NOON, [HEAVY]),
                                           schema=schema))
                self.assertEqual(self.proposed(facts), {HEAVY: ['T_A', 'T_B']})
        for document, needle in [(listing(row('T_A', NOON, [HEAVY]), schema='af/task-list@5'),
                                  "unsupported task list schema 'af/task-list@5'"),
                                 ({'tasks': []}, 'unsupported task list schema None'),
                                 (listing({'task_id': 'T_A'}), 'without a `task_id` and `updated_unix_ms`'),
                                 (listing(dict(row('T_A', NOON, []), gate={'flaky': 'x'})),
                                  'T_A: `gate.flaky` is not a list')]:
            with self.subTest(needle=needle):
                result = self.scan(document, write=True)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn(needle, result.stderr)

    def test_today_comes_from_the_flag_else_the_environment(self):
        rows = listing(row('T_A', NOON, [HEAVY]), row('T_B', NOON, [HEAVY]))
        from_env = self.facts(rows, today=None, env={'AF_TEST_QUARANTINE_TODAY': TODAY})
        self.assertEqual((from_env['today'], from_env['window']['end_unix_ms']), (TODAY, END))
        # The flag wins; a week later both Tasks are outside the window.
        later = self.facts(rows, today='2026-11-06', env={'AF_TEST_QUARANTINE_TODAY': TODAY})
        self.assertEqual((later['today'], later['proposed']), ('2026-11-06', []))
        for today, env, needle in [('29.10.2026', {}, "--today '29.10.2026'"),
                                   (None, {'AF_TEST_QUARANTINE_TODAY': 'soon'}, 'AF_TEST_QUARANTINE_TODAY')]:
            result = self.scan(rows, today=today, env=env)
            self.assertEqual(result.returncode, 1)
            self.assertIn(needle, result.stderr)

    def test_state_runs_the_given_af_and_merges_its_list(self):
        fake = self.dir / 'af'
        fake.write_text(f'#!{sys.executable}\nimport json, sys\n'
                        "assert sys.argv[1:4] == ['task', 'list', '--json'] and sys.argv[4] == '--state'\n"
                        f"print(json.dumps({listing(row('T_B', NOON, [HEAVY]))!r}))\n")
        fake.chmod(0o755)
        facts = self.facts(listing(row('T_A', NOON, [HEAVY])),
                           extra=('--state', str(self.dir / 'store'), '--af', str(fake)))
        self.assertEqual(self.proposed(facts), {HEAVY: ['T_A', 'T_B']})
        failing = self.dir / 'af-failing'
        failing.write_text(f'#!{sys.executable}\nimport sys\nprint("no store", file=sys.stderr)\nsys.exit(2)\n')
        failing.chmod(0o755)
        result = self.scan(extra=('--state', str(self.dir / 'store'), '--af', str(failing)))
        self.assertEqual(result.returncode, 1)
        self.assertIn('exit 2: no store', result.stderr)
        usage = self.scan()  # Neither --list nor --state.
        self.assertEqual(usage.returncode, 2)
        self.assertIn('scan needs at least one --list FILE or --state DIR', usage.stderr)


class Resolution(unittest.TestCase):
    def setUp(self):
        self.resolve = tq.resolver(copy.deepcopy(inventory()), tc.catalog())

    def test_an_exact_identity_and_a_unique_bare_name_resolve(self):
        self.assertEqual(self.resolve(CAPTURE), (CAPTURE, None))
        self.assertEqual(self.resolve('capture::a_settled_worktree_is_admitted_on_retry'), (CAPTURE, None))
        self.assertEqual(self.resolve(SHARED[0]), (SHARED[0], None))

    def test_ambiguous_fixture_ignored_and_feature_only_names_are_rejected(self):
        self.assertEqual(self.resolve('shared::same_name'), (None, 'ambiguous: ' + ', '.join(SHARED)))
        for name in (*FRAGMENTS, IGNORED, FEATURE, 'perf_trace::perf_case', 'capture::',
                     'review-source-git::it capture::a_settled', 'af::it  task_heavy::loaded'):
            with self.subTest(name=name):
                self.assertEqual(self.resolve(name), (None, 'not a test'))


if __name__ == '__main__':
    unittest.main()
