#!/usr/bin/env python3
"""Exercise quarantine scan/confirm/apply with test-local data and an explicit today, plus the
Claw runbook through local command stubs; never this checkout's quarantine file, the clock,
af, cargo, a real af Store, SSH, Docker or sudo."""
import copy
import datetime as dt
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
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
            # The same, with nothing proposed: --write still runs the inventory guard.
            ({'af::it absent::test': held('af::it absent::test')}, [row('T_A', NOON, [HEAVY])],
             'not in the test inventory'),
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


COMMIT = 'c0ffee' * 6 + 'c0ff'
CONFIRMED = (MANY[0], HEAVY, CAPTURE)
# A fake cargo: each confirmation run writes the JUnit its plan entry names (identity -> 'pass',
# 'fail' or 'absent'; null writes none), and every load run writes a failing JUnit of its own.
FAKE_CARGO = r'''
import json, os, sys
from pathlib import Path
import xml.etree.ElementTree as ET
args = sys.argv[1:]
logs = Path(os.environ['FAKE_CARGO_LOGS'])
expression = args[args.index('-E') + 1]
config = Path(args[args.index('--tool-config-file') + 1].split(':', 1)[1]).read_text()
store = Path(json.loads(config.split('dir = ', 1)[1]))
kind = 'load' if 'test(/^' in expression else 'confirm'
with (logs / f'{kind}.log').open('a') as stream:
    stream.write(json.dumps(args) + '\n')
if kind == 'load':
    cases = {'review-graph many::case_01': 'fail'}
else:
    index = len((logs / 'confirm.log').read_text().splitlines())
    cases = json.loads(Path(os.environ['FAKE_CARGO_PLAN']).read_text())[index - 1]
    if cases is None:
        sys.exit(101)
root = ET.Element('testsuites')
for identity, outcome in cases.items():
    if outcome == 'absent':
        continue
    binary, name = identity.split(' ')
    suite = ET.SubElement(root, 'testsuite', name=binary)
    case = ET.SubElement(suite, 'testcase', name=name, classname=binary)
    if outcome == 'fail':
        failure = ET.SubElement(case, 'failure', message='test failed', type='test failure')
        failure.text = f'thread panicked at {name}: run {index if kind == "confirm" else 0}'
        ET.SubElement(case, 'system-err').text = 'stderr of ' + name
(store / 'ci').mkdir(parents=True, exist_ok=True)
ET.ElementTree(root).write(store / 'ci' / 'junit.xml')
sys.exit(100 if 'fail' in cases.values() else 0)
'''


def report(tests, system='Darwin', runs=50, today=TODAY):
    """An af.test-quarantine-confirm/1 report: identity -> its failed runs."""
    def result(failed):
        kind = 'clean' if not failed else 'always_fails' if len(failed) == runs else 'flaky'
        first = {'run': failed[0], 'output': f'thread panicked: ``` run {failed[0]}',
                 'truncated': False} if failed else None
        return {'failures': len(failed), 'failed_runs': list(failed), 'classification': kind,
                'first_failure': first}
    return {'schema': 'af.test-quarantine-confirm/1', 'commit': COMMIT, 'today': today,
            'host': {'system': system, 'machine': 'x86_64', 'cpus': 12, 'test_threads': 12},
            'load': {'selection': 'functional', 'runs': 7}, 'runs': runs,
            'tests': {identity: result(failed) for identity, failed in tests.items()}}


class QuarantineFiles(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name)
        self.inventory = self.dir / 'nextest-list.json'
        self.inventory.write_text(json.dumps(inventory()))
        self.file = self.dir / 'test-quarantine.json'
        tc.write(self.file, {'schema': 'af.test-quarantine/1',
                             'tests': {identity: held(identity) for identity in CONFIRMED}})

    def environment(self, **extra):
        environment = {k: v for k, v in os.environ.items() if not k.startswith(('AF_', 'FAKE_'))}
        environment.update(extra)
        return environment


class Confirm(QuarantineFiles):
    def setUp(self):
        super().setUp()
        self.bin = self.dir / 'bin'
        self.bin.mkdir()
        self.cargo = self.bin / 'cargo'
        self.cargo.write_text(f'#!{sys.executable} -I\n{FAKE_CARGO}')
        git = self.bin / 'git'
        git.write_text(f'#!{sys.executable} -I\nimport sys\n'
                       "assert sys.argv[1:] == ['rev-parse', 'HEAD']\n"
                       f'print({COMMIT!r})\n')
        for path in (self.cargo, git):
            path.chmod(0o755)
        self.logs = self.dir / 'logs'
        self.logs.mkdir()
        self.report = self.dir / 'report.json'

    def confirm(self, plan, *extra, load='none'):
        (self.dir / 'plan.json').write_text(json.dumps(plan))
        for log in self.logs.iterdir():
            log.unlink()
        self.report.unlink(missing_ok=True)
        command = [sys.executable, '-I', str(SCRIPT), 'confirm', '--report', str(self.report),
                   '--runs', str(len(plan)), '--test-threads', '3', '--load', load,
                   '--cargo', str(self.cargo), '--inventory', str(self.inventory),
                   '--quarantine', str(self.file), '--today', TODAY, *extra]
        return subprocess.run(command, capture_output=True, text=True, env=self.environment(
            PATH=f'{self.bin}{os.pathsep}{os.environ.get("PATH", "")}',
            FAKE_CARGO_LOGS=str(self.logs), FAKE_CARGO_PLAN=str(self.dir / 'plan.json')))

    def runs(self, kind='confirm'):
        log = self.logs / f'{kind}.log'
        return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []

    @staticmethod
    def plan(runs=5):
        """Run by run: MANY[0] fails in runs 2 and 4, HEAVY in all, CAPTURE in none."""
        return [{MANY[0]: 'fail' if index in (2, 4) else 'pass', HEAVY: 'fail', CAPTURE: 'pass'}
                for index in range(1, runs + 1)]

    def test_every_run_runs_and_each_identity_is_classified(self):
        result = self.confirm(self.plan())
        self.assertEqual(result.returncode, 0, result.stderr)
        value = tc.read(self.report)
        self.assertEqual(value['tests'][MANY[0]], {
            'failures': 2, 'failed_runs': [2, 4], 'classification': 'flaky',
            'first_failure': {'run': 2, 'output': 'thread panicked at many::case_00: run 2\n'
                                                  '--- system-err ---\nstderr of many::case_00',
                              'truncated': False}})
        self.assertEqual({i: (r['classification'], r['failed_runs']) for i, r in value['tests'].items()
                          if i != MANY[0]},
                         {HEAVY: ('always_fails', [1, 2, 3, 4, 5]), CAPTURE: ('clean', [])})
        self.assertIsNone(value['tests'][CAPTURE]['first_failure'])
        self.assertEqual((value['schema'], value['commit'], value['today'], value['runs'], value['load']),
                         ('af.test-quarantine-confirm/1', COMMIT, TODAY, 5,
                          {'selection': 'none', 'runs': 0}))
        self.assertEqual(value['host'], {'system': tq.platform.system(), 'machine': tq.platform.machine(),
                                         'cpus': os.cpu_count(), 'test_threads': 3})
        # The report has the shape its generated schema names.
        schema = tc.schema_documents()['test-quarantine-confirm-v1.json']
        self.assertEqual(set(value), set(schema['required']))
        self.assertEqual(set(value['tests'][HEAVY]),
                         set(schema['properties']['tests']['additionalProperties']['required']))
        # Five whole runs of exactly the held identities: no early stop, no retry, no rerun alone.
        runs = self.runs()
        self.assertEqual(len(runs), 5)
        exact = ('(binary_id(=af::it) & test(=task_heavy::loaded)) | (binary_id(=review-graph) & '
                 'test(=many::case_00)) | (binary_id(=review-source-git::it) & '
                 'test(=capture::a_settled_worktree_is_admitted_on_retry))')
        for args in runs:
            self.assertEqual(args[:13], ['nextest', 'run', '--workspace', '--locked', '--profile', 'ci',
                                         '--no-fail-fast', '--retries', '0', '--test-threads', '3',
                                         '-E', exact])
        self.assertEqual(self.runs('load'), [])
        self.assertIn(f'flaky: {MANY[0]} (failed in 2 of 5 runs)', result.stdout)
        # The report is never overwritten.
        before = self.report.read_bytes()
        again = subprocess.run([sys.executable, '-I', str(SCRIPT), 'confirm', '--report',
                                str(self.report), '--quarantine', str(self.file), '--today', TODAY,
                                '--cargo', str(self.cargo)], capture_output=True, text=True,
                               env=self.environment())
        self.assertEqual(again.returncode, 1)
        self.assertIn('exists; fix: name a new report file', again.stderr)
        self.assertEqual(self.report.read_bytes(), before)

    def test_only_restricts_the_runs_to_the_named_entries(self):
        plan = [{HEAVY: 'pass'}, {HEAVY: 'fail'}]
        result = self.confirm(plan, '--only', HEAVY)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(list(tc.read(self.report)['tests']), [HEAVY])
        self.assertEqual({args[12] for args in self.runs()},
                         {'(binary_id(=af::it) & test(=task_heavy::loaded))'})
        absent = self.confirm(plan, '--only', 'af::it absent::test')
        self.assertEqual(absent.returncode, 1)
        self.assertIn('--only af::it absent::test is not in', absent.stderr)
        self.assertFalse(self.report.exists())

    def test_a_run_without_a_junit_or_without_an_identity_writes_no_report(self):
        cases = [(3, None, 'run 3 of 5 ended without a JUnit (nextest exit 101)'),
                 (2, {MANY[0]: 'pass', HEAVY: 'fail', CAPTURE: 'absent'},
                  f'run 2 of 5: its JUnit has no result for {CAPTURE}')]
        for index, run, needle in cases:
            with self.subTest(needle=needle):
                plan = self.plan()
                plan[index - 1] = run
                result = self.confirm(plan)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn(needle, result.stderr)
                self.assertIn('no report written: a missing result is not a pass', result.stderr)
                self.assertFalse(self.report.exists())
                self.assertEqual(len(self.runs()), index)

    def test_the_load_runs_the_functional_class_without_the_held_tests_and_never_counts(self):
        result = self.confirm(self.plan(3), load='functional')
        self.assertEqual(result.returncode, 0, result.stderr)
        value = tc.read(self.report)
        loads = self.runs('load')
        self.assertGreaterEqual(len(loads), 1)
        # The report counts the load runs started; the last may end before its fake logs.
        self.assertEqual(value['load']['selection'], 'functional')
        self.assertIn(value['load']['runs'], (len(loads), len(loads) + 1))
        expected = tc.filterset(tc.partition(inventory(), tc.catalog(),
                                             {i: held(i) for i in CONFIRMED}), ['functional'])
        for args in loads:
            self.assertEqual(args[:11], ['nextest', 'run', '--workspace', '--locked', '--profile',
                                         'ci', '--no-fail-fast', '--retries', '0', '--test-threads', '3'])
            self.assertEqual(args[11:13], ['-E', expected])
        # The functional siblings run; no held identity does, the functional MANY[0] included.
        self.assertIn('many::case_01$', expected)
        for identity in CONFIRMED:
            self.assertNotIn(identity.split(' ')[1], expected)
        # Every load run failed, and the classifications are those of a run without load.
        with_load = {i: (r['classification'], r['failed_runs']) for i, r in value['tests'].items()}
        self.assertEqual(self.confirm(self.plan(3)).returncode, 0)
        without = {i: (r['classification'], r['failed_runs'])
                   for i, r in tc.read(self.report)['tests'].items()}
        self.assertEqual(with_load, without)
        self.assertEqual(self.runs('load'), [])


class Apply(QuarantineFiles):
    def setUp(self):
        super().setUp()
        self.issues = self.dir / 'issues'

    def apply(self, value, *extra, write=True, issues=True, today=TODAY, env=None):
        path = self.dir / 'report.json'
        path.write_text(json.dumps(value))
        command = [sys.executable, '-I', str(SCRIPT), 'apply', str(path), '--inventory',
                   str(self.inventory), '--quarantine', str(self.file), *extra]
        command += ['--today', today] if today else []
        command += ['--write'] if write else []
        command += ['--issues', str(self.issues)] if issues else []
        return subprocess.run(command, capture_output=True, text=True, env=self.environment(**(env or {})))

    def entries(self):
        return tc.read(self.file)['tests']

    def test_flaky_always_fails_and_clean_outcomes(self):
        value = report({MANY[0]: [2, 4], HEAVY: list(range(1, 51)), CAPTURE: []})
        result = self.apply(value)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.entries(), {MANY[0]: {'state': 'flaky', 'since': TODAY,
                                                    'expires': '2026-11-12', 'evidence': ['T_OLD']}})
        self.assertEqual(sorted(p.name for p in self.issues.iterdir()),
                         ['flaky-review-graph-many-case_00.md', 'p0-af-it-task_heavy-loaded.md'])
        self.assertIn(f'flaky: {MANY[0]} (failed in 2 of 50 runs): set state flaky, since {TODAY}, '
                      'expires 2026-11-12', result.stdout)
        self.assertIn(f'always_fails: {HEAVY} (failed in 50 of 50 runs): removed the entry', result.stdout)
        self.assertIn(f'clean: {CAPTURE} (failed in 0 of 50 runs): removed the entry', result.stdout)
        self.assertIn(f"gh issue create --title 'flaky: {MANY[0]}' --label flaky --body-file "
                      f"'{self.issues / 'flaky-review-graph-many-case_00.md'}'", result.stdout)
        self.assertIn(f"gh issue create --title 'P0: {HEAVY} fails in every run' --label P0",
                      result.stdout)

    def test_an_entry_already_flaky_keeps_its_issue_and_evidence(self):
        tc.write(self.file, {'schema': 'af.test-quarantine/1', 'tests': {HEAVY: {
            **held(HEAVY), 'state': 'flaky', 'evidence': ['T_A', 'T_B'], 'issue': 470}}})
        self.assertEqual(self.apply(report({HEAVY: [9]})).returncode, 0)
        self.assertEqual(self.entries(), {HEAVY: {'state': 'flaky', 'since': TODAY, 'expires': '2026-11-12',
                                                  'evidence': ['T_A', 'T_B'], 'issue': 470}})

    def test_the_issue_bodies_hold_the_output_the_runs_the_host_and_the_evidence(self):
        tc.write(self.file, {'schema': 'af.test-quarantine/1', 'tests': {
            MANY[0]: {**held(MANY[0]), 'evidence': ['T_A', 'T_B']}, HEAVY: held(HEAVY)}})
        self.assertEqual(self.apply(report({MANY[0]: [2, 4], HEAVY: list(range(1, 51))},
                                           system='Linux')).returncode, 0)
        flaky = (self.issues / 'flaky-review-graph-many-case_00.md').read_text()
        self.assertIn(f'`{MANY[0]}` failed in 2 of 50 runs', flaky)
        self.assertIn('- Failing runs: 2 of 50 (runs 2, 4).', flaky)
        self.assertIn(f'- Commit: `{COMMIT}`, confirmed on {TODAY}.', flaky)
        self.assertIn('- Host: Linux x86_64, 12 CPUs, 12 test threads; load: functional (7 runs', flaky)
        self.assertIn('- Evidence Tasks (their gates listed it flaky): `T_A`, `T_B`.', flaky)
        # The output sits in a fence longer than the backticks inside it.
        self.assertIn('First failure, run 2:\n\n````text\nthread panicked: ``` run 2\n````\n', flaky)
        self.assertIn('until 2026-11-12', flaky)
        p0 = (self.issues / 'p0-af-it-task_heavy-loaded.md').read_text()
        self.assertTrue(p0.startswith(f'P0: main is broken. `{HEAVY}` failed in every one of 50 runs'))
        self.assertIn('- Failing runs: 50 of 50 (runs 1, 2, 3', p0)
        self.assertIn('- Evidence Tasks (their gates listed it flaky): `T_OLD`.', p0)
        self.assertIn('````text\nthread panicked: ``` run 1\n````', p0)

    def test_a_clean_report_releases_an_entry_only_from_the_evidence_platform(self):
        cases = [('Darwin', (), True), ('Linux', (), False),
                 ('Linux', ('--evidence-platform', 'linux'), True),
                 ('Darwin', ('--evidence-platform', 'linux'), False)]
        for system, extra, released in cases:
            with self.subTest(system=system, extra=extra):
                tc.write(self.file, {'schema': 'af.test-quarantine/1', 'tests': {
                    CAPTURE: held(CAPTURE), HEAVY: held(HEAVY)}})
                before = self.file.read_bytes()
                result = self.apply(report({CAPTURE: []}, system=system), *extra)
                self.assertEqual(result.returncode, 0, result.stderr)
                if released:
                    self.assertEqual(self.entries(), {HEAVY: held(HEAVY)})
                    self.assertIn(f'clean: {CAPTURE} (failed in 0 of 50 runs): removed the entry',
                                  result.stdout)
                else:
                    self.assertEqual(self.file.read_bytes(), before)
                    evidence = 'Linux' if extra else 'Darwin'
                    self.assertIn(f'clean: {CAPTURE} (failed in 0 of 50 runs): keeps the entry '
                                  f'quarantined, its dates unchanged: the report is from {system} and '
                                  f'its evidence from {evidence} gates', result.stdout)
                self.assertFalse(self.issues.exists())

    def test_without_write_nothing_changes(self):
        before = self.file.read_bytes()
        files = set(self.dir.iterdir())
        result = self.apply(report({MANY[0]: [2, 4], HEAVY: list(range(1, 51)), CAPTURE: []}),
                            write=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.file.read_bytes(), before)
        self.assertEqual(set(self.dir.iterdir()) - files, {self.dir / 'report.json'})
        self.assertIn(f'flaky: {MANY[0]} (failed in 2 of 50 runs): would set state flaky', result.stdout)
        self.assertIn(f'would write the flaky issue body {self.issues / "flaky-review-graph-many-case_00.md"}',
                      result.stdout)
        self.assertIn('nothing written; rerun with --write --issues DIR to apply it', result.stdout)
        self.assertNotIn('gh issue create', result.stdout)

    def test_refusals_write_nothing(self):
        later = report({HEAVY: [3]}, today='2026-10-30')
        broken = report({HEAVY: [3]})
        broken['tests'][HEAVY]['classification'] = 'clean'
        cases = [
            # A report from a later day than apply's today.
            (later, {}, 'the report is from 2026-10-30, later than today (2026-10-29)'),
            # An identity no longer in the file.
            (report({'af::it absent::test': []}), {},
             'apply: af::it absent::test is in the report but not in the quarantine file'),
            # Not a confirm report, or one whose classification does not follow from its runs.
            ({**broken, 'schema': 'af.test-quarantine-confirm/2'}, {},
             "not an af.test-quarantine-confirm/1 report (schema 'af.test-quarantine-confirm/2')"),
            (broken, {}, "classification 'clean' for 1 of 50 failed runs, expected 'flaky'"),
            # Issues to file but no directory for their bodies.
            (report({HEAVY: [3]}), {'issues': False}, 'name the directory for their bodies with --issues'),
            # A guard refusal: the result fails the partition-time inventory guard.
            (report({HEAVY: [3], CAPTURE: []}), {'absent': True}, 'not in the test inventory'),
        ]
        for value, options, needle in cases:
            with self.subTest(needle=needle):
                tests = {identity: held(identity) for identity in CONFIRMED}
                if options.get('absent'):
                    tests['af::it absent::test'] = held('af::it absent::test')
                tc.write(self.file, {'schema': 'af.test-quarantine/1', 'tests': tests})
                before = self.file.read_bytes()
                result = self.apply(value, issues=options.get('issues', True))
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn(needle, result.stderr)
                self.assertEqual(result.stdout, '')
                self.assertEqual(self.file.read_bytes(), before)
                self.assertFalse(self.issues.exists())
        # An issue body already there is never overwritten.
        self.issues.mkdir()
        (self.issues / 'flaky-af-it-task_heavy-loaded.md').write_text('mine')
        before = self.file.read_bytes()
        result = self.apply(report({HEAVY: [3]}))
        self.assertEqual(result.returncode, 1)
        self.assertIn('flaky-af-it-task_heavy-loaded.md exists', result.stderr)
        self.assertEqual(((self.issues / 'flaky-af-it-task_heavy-loaded.md').read_text(),
                          self.file.read_bytes()), ('mine', before))

    def test_today_comes_from_the_flag_else_the_environment(self):
        result = self.apply(report({HEAVY: [3]}), today=None,
                            env={'AF_TEST_QUARANTINE_TODAY': '2026-10-31'})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.entries()[HEAVY]['since'], '2026-10-31')
        self.assertEqual(self.entries()[HEAVY]['expires'], '2026-11-14')


# Execute the runbook's remote shell bodies locally. Every privileged/network command is a
# fake except local Git object reads; ownership is a receipt, not a host chown. Host Python
# imports the archived production package with temporary coordination paths and simulated root uid.
RUNBOOK_STUB = r'''
import builtins
import fcntl
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile

sys.dont_write_bytecode = True
name, args = Path(sys.argv[0]).name, sys.argv[1:]
home = Path(os.environ['HOME'])
work = home / ('af-quarantine-' + os.environ['COMMIT'])
owner = work / 'owner'
lock = home / 'af-perf-machine.lock'
with (home / 'events').open('a') as output:
    output.write(json.dumps([name, args]) + '\n')
if name == 'id':
    assert args == ['-u']
    print(os.environ['FAKE_UID'])
elif name == 'sudo':
    assert os.environ['FAKE_UID'] not in ('0', '1000')
    assert args == ['chown', '1000:1000', str(work / 'run')]
    os.environ['FAKE_ELEVATED'] = '1'
    os.execvp(args[0], args)
elif name == 'chown':
    assert args == ['1000:1000', str(work / 'run')]
    assert (work / 'run').is_dir()
    if os.environ['FAKE_UID'] != '0' and os.environ.get('FAKE_ELEVATED') != '1':
        sys.exit('chown: Operation not permitted')
    owner.write_text('1000')
elif name == 'git':
    if args[0] == '--no-replace-objects':
        args = args[1:]
        assert args[:2] == ['-c', 'core.fsmonitor=false']
        args = args[2:]
    else:
        assert args[-2:] == ['rev-parse', 'HEAD'], 'host object reads must disable replacements'
    assert args[:4] == ['-c', 'safe.directory=*', '-C', str(work / 'run/src')]
    command = args[4:]
    assert command in (['rev-parse', 'HEAD'],
                      ['diff-index', '--cached', '--no-ext-diff', '--no-textconv',
                       '--name-only', os.environ['COMMIT'], '--',
                       'scripts/performance/perflab'],
                      ['archive', '--format=tar', os.environ['COMMIT'],
                       'scripts/performance/perflab'])
    command_line = [os.environ['RUNBOOK_GIT'], '--no-replace-objects',
                    '-c', 'core.fsmonitor=false'] + args
    if command[0] == 'archive' and os.environ.get('FAKE_ARCHIVE_MEMBER'):
        data = subprocess.check_output(command_line)
        output = io.BytesIO()
        with tarfile.open(fileobj=io.BytesIO(data)) as original, \
                tarfile.open(fileobj=output, mode='w') as modified:
            kind = os.environ['FAKE_ARCHIVE_MEMBER']
            for member in original:
                if (kind == 'missing-init' and member.name.endswith('/__init__.py')
                        or kind == 'missing-hostlock' and member.name.endswith('/hostlock.py')):
                    continue
                modified.addfile(member, original.extractfile(member) if member.isfile() else None)
            member = tarfile.TarInfo('scripts/performance/perflab/injected')
            if kind in ('symlink', 'hardlink'):
                member.type = tarfile.SYMTYPE if kind == 'symlink' else tarfile.LNKTYPE
                member.linkname = str(home / 'outside')
            elif kind == 'absolute':
                member.name = str(home / 'outside')
            elif kind == 'traversal':
                member.name = 'scripts/performance/perflab/../../../../outside'
            elif kind == 'outside-directory':
                member.name, member.type = 'outside-directory', tarfile.DIRTYPE
            elif kind not in ('missing-init', 'missing-hostlock'):
                assert kind == 'outside-file'
                member.name = 'scripts/performance/outside.py'
            if kind not in ('missing-init', 'missing-hostlock'):
                modified.addfile(member)
        sys.stdout.buffer.write(output.getvalue())
    else:
        os.execv(command_line[0], command_line)
elif name == 'nohup':
    assert args[0] == 'python3'
    os.execvp(args[0], args)
elif name == 'python3':
    from unittest import mock
    assert args == ['-I', '-B', '-', str(work), os.environ['COMMIT']]
    source = sys.stdin.read()
    original_import = builtins.__import__

    def instrument_import(name, globals=None, locals=None, fromlist=(), level=0):
        package = original_import(name, globals, locals, fromlist, level)
        if name != 'perflab' or 'hostlock' not in fromlist:
            return package
        hostlock = package.hostlock
        imported = Path(hostlock.__file__).resolve()
        assert (work / 'run').resolve() not in imported.parents, 'mutable host import'
        assert imported.parents[3].parent == work.resolve(), 'authority must be host-only'
        assert imported.parents[3].stat().st_mode & 0o777 == 0o700
        with (home / 'imports').open('a') as output:
            output.write(str(imported) + '\n')
        anchor = home / 'anchor.json'
        hostlock.MACHINE_LOCK_ANCHOR = str(anchor)
        hostlock.MACHINE_LOCK_DIRECTORY = str(home)
        read_anchor = hostlock.read_anchor
        check_directory = hostlock._check_directory
        fstat, lstat, flock = os.fstat, os.lstat, fcntl.flock

        def anchor_stat(fd):
            info = fstat(fd)
            named = anchor.lstat()
            if (info.st_dev, info.st_ino) == (named.st_dev, named.st_ino):
                fields = list(info)
                fields[4] = int(os.environ.get('FAKE_ANCHOR_UID', '0'))
                return os.stat_result(fields)
            return info

        def anchored_read(required=False):
            with (home / 'anchor-reads').open('a') as output:
                output.write(json.dumps(required) + '\n')
            with mock.patch.object(hostlock.os, 'fstat', anchor_stat):
                identity = read_anchor(required=required)
            if required and os.environ.get('FAKE_REMOVE_ANCHOR_LOCK'):
                anchor.unlink()
                lock.unlink()
            return identity

        def root_directory_stat(path):
            info = lstat(path)
            assert path == str(home.resolve())
            fields = list(info)
            fields[4] = 0
            return os.stat_result(fields)

        def system_directory(directory, system_owned):
            assert system_owned, 'production system-directory validation must remain active'
            with mock.patch.object(hostlock.os, 'lstat', root_directory_stat):
                check_directory(directory, system_owned)

        def acquired(fd, operation):
            assert operation == fcntl.LOCK_EX | fcntl.LOCK_NB
            flock(fd, operation)
            # Introduce a fence only after acquisition: a pre-lock check cannot pass this test.
            fence = Path(str(lock) + '.fence')
            kind = os.environ.get('FAKE_FENCE')
            if kind == 'directory':
                fence.mkdir()
            elif kind == 'file':
                fence.write_text('uncertain experiment')
            elif kind == 'symlink':
                fence.symlink_to(home / 'absent')
            if os.environ.get('FAKE_REPLACE_LOCK'):
                lock.rename(home / 'held-lock')
                lock.write_text('replacement')

        hostlock.read_anchor = anchored_read
        hostlock._check_directory = system_directory
        hostlock.fcntl.flock = acquired
        return package

    with mock.patch.object(builtins, '__import__', instrument_import):
        sys.argv = ['-', str(work), os.environ['COMMIT']]
        exec(compile(source, '<runbook launch>', 'exec'), {'__name__': '__main__'})
elif name == 'docker':
    assert (owner.read_text() if owner.exists() else os.environ['FAKE_UID']) == '1000'
    assert args[:2] == ['run', '--rm']
    if '--name' in args:
        assert args == ['run', '--rm', '--name', 'afq-confirm', '-v', str(work / 'run') + ':/work',
                        '-w', '/work/src', 'af-quarantine-confirm:1.88.0', 'python3',
                        'scripts/test-quarantine.py', 'confirm', '--report', '/work/confirm.json']
        with lock.open('r') as probe:
            try:
                fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                pass
            else:
                sys.exit('Docker reached without the machine lock')
        assert not os.path.lexists(str(lock) + '.fence')
        if os.environ.get('FAKE_DIRTY_HOSTLOCK'):
            (work / 'run/src/scripts/performance/perflab/hostlock.py').write_text(
                'raise RuntimeError("MUTABLE AUTHORITY IMPORTED")\n')
        if os.environ.get('FAKE_AUTHORITY_LINK'):
            imported = Path((home / 'imports').read_text().splitlines()[-1])
            (imported.parent / 'cleanup-link').symlink_to(home / 'outside')
        print('fake confirmation launched')
    else:
        assert args[2:9] == ['-v', str(work / 'run') + ':/work', '-w', '/work',
                             'af-quarantine-confirm:1.88.0', 'sh', '-c']
        assert 'git clone https://github.com/PakhomovAlexander/afactory.git src &&' in args[9]
        assert 'git -C src checkout --detach ' + os.environ['COMMIT'] in args[9]
        # A repeated clone must fail, as the real one does. This is a local object fixture.
        shutil.copytree(os.environ['RUNBOOK_REPO'], work / 'run/src')
else:
    sys.exit('unexpected stub: ' + name)
'''


class ClawRunbook(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        tmp = tempfile.TemporaryDirectory(prefix='afq-runbook-source-')
        cls.addClassCleanup(tmp.cleanup)
        cls.repo = Path(tmp.name)
        cls.git = shutil.which('git')
        package = cls.repo / 'scripts/performance/perflab'
        shutil.copytree(ROOT / 'scripts/performance/perflab', package,
                        ignore=shutil.ignore_patterns('__pycache__', '*.pyc'))
        cls.git_env = {'HOME': str(cls.repo), 'PATH': '/usr/bin:/bin',
                       'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': os.devnull}
        for args in (['init', '-q'], ['add', '.'],
                     ['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
                      '-c', 'commit.gpgsign=false', 'commit', '-qm', 'authority fixture']):
            subprocess.run([cls.git, '-C', str(cls.repo), *args], env=cls.git_env,
                           check=True, capture_output=True)
        cls.commit = subprocess.check_output(
            [cls.git, '-C', str(cls.repo), 'rev-parse', 'HEAD'], env=cls.git_env, text=True).strip()

    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix='afq-runbook-')
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name).resolve()
        self.bin = self.dir / 'bin'
        self.bin.mkdir()
        for name in ('id', 'sudo', 'chown', 'git', 'nohup', 'python3', 'docker'):
            path = self.bin / name
            path.write_text(f'#!{sys.executable} -I\n{RUNBOOK_STUB}')
            path.chmod(0o755)
        # No host command search: real Docker, sudo, SSH and Provider CLIs are unreachable.
        for name in ('sh', 'mkdir'):
            (self.bin / name).symlink_to('/bin/' + name)
        self.doc = (ROOT / 'docs/test-quarantine-claw.md').read_text()
        self.blocks = re.findall(
            r'^ssh claw-3 COMMIT="\$COMMIT" bash -s <<\'CLAW\'\n(.*?)^CLAW$',
            self.doc, re.MULTILINE | re.DOTALL)
        self.assertTrue(self.blocks, 'quoted COMMIT remote here-document is required')
        self.work = self.dir / f'af-quarantine-{self.commit}'
        self.lock = self.dir / 'af-perf-machine.lock'
        self.lock.write_text('')
        self.lock.chmod(0o644)
        info = self.lock.stat()
        self.anchor = self.dir / 'anchor.json'
        self.anchor.write_text(json.dumps({'schema': 'af.perf-machine-lock-anchor/1',
                                           'device': info.st_dev, 'inode': info.st_ino}))
        self.anchor.chmod(0o644)
        self.fence = Path(str(self.lock) + '.fence')

    def run_block(self, block, uid='1000', fence='', extra_env=None):
        if 'nohup' in block:
            block += '\nwait "$!"\n'  # Join the detached stub deterministically; no sleeps.
        return subprocess.run(['/bin/bash', '-s'], input=block, capture_output=True, text=True,
                              env={'HOME': str(self.dir), 'PATH': str(self.bin),
                                   'COMMIT': self.commit,
                                   'FAKE_UID': uid, 'FAKE_FENCE': fence,
                                   'RUNBOOK_REPO': str(self.repo), 'RUNBOOK_GIT': self.git,
                                   'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': os.devnull,
                                   **(extra_env or {})})

    def events(self):
        return [json.loads(line) for line in (self.dir / 'events').read_text().splitlines()]

    def prepare(self, uid='1000'):
        result = self.run_block(self.blocks[0], uid=uid)
        self.assertEqual(result.returncode, 0, result.stderr)

    def assert_launch_refused(self, needle, extra_env=None):
        before = len([e for e in self.events() if e[0] == 'docker'])
        result = self.run_block(self.blocks[-1], extra_env=extra_env)
        self.assertEqual(result.returncode, 1, result.stderr)
        log = (self.work / 'confirm.log').read_text()
        self.assertIn(needle, log)
        self.assertIn('operator provisioning/recovery', log)
        self.assertEqual(len([e for e in self.events() if e[0] == 'docker']), before)

    def test_missing_anchored_lock_refuses_before_docker_without_creating_a_lock(self):
        self.prepare()
        self.lock.unlink()
        anchor = self.anchor.read_bytes()
        self.assert_launch_refused('cannot open the machine lock')
        self.assertFalse(self.lock.exists())
        self.assertEqual(self.anchor.read_bytes(), anchor)

    def test_anchor_and_lock_disappearing_after_required_read_never_create_or_launch(self):
        self.prepare()
        self.assert_launch_refused('cannot open the machine lock',
                                   extra_env={'FAKE_REMOVE_ANCHOR_LOCK': '1'})
        self.assertEqual((self.dir / 'anchor-reads').read_text().splitlines(), ['true'])
        self.assertFalse(self.anchor.exists())
        self.assertFalse(self.lock.exists())
        self.assertEqual(list(self.work.glob('host-authority-*')), [])

    def test_previous_confirmation_dirtying_hostlock_refuses_before_import_or_docker(self):
        self.prepare()
        # Git status can be fooled by this index flag; compare actual package bytes instead.
        subprocess.run([self.git, '-C', str(self.work / 'run/src'), 'update-index',
                        '--assume-unchanged', 'scripts/performance/perflab/hostlock.py'],
                       env=self.git_env, check=True)
        result = self.run_block(self.blocks[-1], extra_env={'FAKE_DIRTY_HOSTLOCK': '1'})
        self.assertEqual(result.returncode, 0, result.stderr)
        imports = (self.dir / 'imports').read_bytes()
        before = len([e for e in self.events() if e[0] == 'docker'])
        result = self.run_block(self.blocks[-1])
        self.assertEqual(result.returncode, 1, result.stderr)
        log = (self.work / 'confirm.log').read_text()
        self.assertIn('dirty host authority source', log)
        self.assertNotIn('MUTABLE AUTHORITY IMPORTED', log)
        self.assertEqual((self.dir / 'imports').read_bytes(), imports)
        self.assertEqual(len([e for e in self.events() if e[0] == 'docker']), before)
        self.assertEqual(list(self.work.glob('host-authority-*')), [])

    def test_dirty_package_init_staged_untracked_and_ignored_authority_refuse_before_import(self):
        self.prepare()
        package = self.work / 'run/src/scripts/performance/perflab'
        for kind in ('init', 'staged', 'untracked', 'ignored', 'symlink', 'fifo', 'missing'):
            with self.subTest(kind=kind):
                name = ('__init__.py' if kind == 'init' else 'hostlock.py'
                        if kind in ('symlink', 'fifo', 'missing') else 'injected.py')
                path = package / name
                before = path.read_bytes() if path.exists() else None
                if kind in ('symlink', 'fifo', 'missing'):
                    path.unlink()
                    if kind == 'symlink':
                        path.symlink_to(self.dir / 'absent-source')
                    elif kind == 'fifo':
                        os.mkfifo(path)
                else:
                    path.write_text('raise RuntimeError("MUTABLE AUTHORITY IMPORTED")\n')
                repo = self.work / 'run/src'
                if kind == 'staged':
                    subprocess.run([self.git, '-C', str(repo), 'add', str(path)],
                                   env=self.git_env, check=True)
                if kind == 'ignored':
                    (repo / '.git/info/exclude').write_text('injected.py\n')
                result = self.run_block(self.blocks[-1])
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn('dirty host authority source', (self.work / 'confirm.log').read_text())
                self.assertFalse((self.dir / 'imports').exists())
                self.assertEqual([e[1][:2] for e in self.events() if e[0] == 'docker'],
                                 [['run', '--rm']])  # Preparation alone.
                if kind == 'staged':
                    subprocess.run([self.git, '-C', str(repo), 'reset', '-q', 'HEAD', '--', str(path)],
                                   env=self.git_env, check=True)
                path.unlink(missing_ok=True)
                if before is not None:
                    path.write_bytes(before)

    def test_fresh_host_only_authority_cleanup_never_follows_links_or_deletes_outside(self):
        self.prepare()
        outside = self.dir / 'outside'
        outside.mkdir()
        sentinel = outside / 'sentinel'
        sentinel.write_text('keep me')
        prior = self.work / 'host-authority-prior'
        prior.mkdir()
        (prior / 'sentinel').write_text('previous state')
        for _ in range(2):
            result = self.run_block(self.blocks[-1], extra_env={'FAKE_AUTHORITY_LINK': '1'})
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(sentinel.read_text(), 'keep me')
            self.assertEqual((prior / 'sentinel').read_text(), 'previous state')
            self.assertEqual(list(self.work.glob('host-authority-*')), [prior])
        imports = (self.dir / 'imports').read_text().splitlines()
        self.assertEqual(len(imports), 2)
        self.assertNotEqual(imports[0], imports[1])
        self.assertTrue(all(not Path(path).exists() for path in imports))

    def test_archive_links_and_paths_outside_the_package_refuse_without_import_or_escape(self):
        self.prepare()
        outside = self.dir / 'outside'
        outside.write_text('untouched')
        for kind in ('symlink', 'hardlink', 'absolute', 'traversal',
                     'outside-file', 'outside-directory', 'missing-init', 'missing-hostlock'):
            with self.subTest(kind=kind):
                result = self.run_block(self.blocks[-1],
                                        extra_env={'FAKE_ARCHIVE_MEMBER': kind})
                self.assertEqual(result.returncode, 1, result.stderr)
                needle = ('incomplete host authority archive' if kind.startswith('missing-')
                          else 'unsafe host authority archive member')
                self.assertIn(needle,
                              (self.work / 'confirm.log').read_text())
                self.assertFalse((self.dir / 'imports').exists())
                self.assertEqual(outside.read_text(), 'untouched')
                self.assertFalse((self.work / 'outside-directory').exists())
                self.assertEqual(list(self.work.glob('host-authority-*')), [])
                self.assertEqual(len([e for e in self.events() if e[0] == 'docker']), 1)

    def test_authority_cleanliness_never_executes_checkout_filters_or_fsmonitor(self):
        self.prepare()
        trap = self.bin / 'git-hook-trap'
        marker = self.dir / 'hook-executed'
        trap.write_text(f'#!{sys.executable} -I\n'
                        f'from pathlib import Path\nPath({str(marker)!r}).touch()\n')
        trap.chmod(0o755)
        repo = self.work / 'run/src'
        for key in ('core.fsmonitor', 'filter.trap.clean'):
            subprocess.run([self.git, '-C', str(repo), 'config', key, str(trap)],
                           env=self.git_env, check=True)
        (repo / '.gitattributes').write_text('*.py filter=trap\n')
        result = self.run_block(self.blocks[-1])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(marker.exists())

    def test_replaced_lock_refuses_the_anchor_mismatch_before_docker_without_repinning(self):
        self.prepare()
        anchor = self.anchor.read_bytes()
        original = self.dir / 'original-lock'
        self.lock.rename(original)
        self.lock.write_text('replacement')
        self.assert_launch_refused('inode differs from the root-owned anchor')
        self.assertEqual(self.lock.read_text(), 'replacement')
        self.assertEqual(self.anchor.read_bytes(), anchor)
        self.assertTrue(original.exists())

    def test_replacement_during_acquisition_refuses_the_named_vs_held_inode_before_docker(self):
        self.prepare()
        anchor = self.anchor.read_bytes()
        self.assert_launch_refused('no longer names the locked file',
                                   extra_env={'FAKE_REPLACE_LOCK': '1'})
        self.assertEqual(self.lock.read_text(), 'replacement')
        self.assertTrue((self.dir / 'held-lock').exists())
        self.assertEqual(self.anchor.read_bytes(), anchor)

    def test_symlinked_lock_including_dangling_refuses_before_docker_without_following(self):
        self.prepare()
        target = self.dir / 'lock-target'
        self.lock.rename(target)
        for destination in (target, self.dir / 'absent'):
            with self.subTest(destination=destination.name):
                self.lock.symlink_to(destination)
                self.assert_launch_refused('cannot open the machine lock')
                self.assertTrue(self.lock.is_symlink())
                self.assertEqual(target.read_bytes(), b'')
                self.assertFalse((self.dir / 'absent').exists())
                self.lock.unlink()

    def test_unsafe_lock_mode_or_second_link_refuses_before_docker_without_repair(self):
        self.prepare()
        self.lock.chmod(0o666)
        self.assert_launch_refused('writable by others')
        self.assertEqual(self.lock.stat().st_mode & 0o777, 0o666)
        self.lock.chmod(0o644)
        os.link(self.lock, self.dir / 'second-link')
        self.assert_launch_refused('has 2 links, not one')
        self.assertEqual(self.lock.stat().st_nlink, 2)

    def test_missing_unsafe_or_mismatched_anchor_refuses_before_docker_without_repair(self):
        self.prepare()
        anchor = self.anchor.read_bytes()
        info = self.lock.stat()
        for kind, needle in (('missing', 'requires root-owned'),
                             ('owner', 'unsafe root-owned machine lock anchor'),
                             ('mode', 'unsafe root-owned machine lock anchor'),
                             ('symlink', 'machine lock anchor refused'),
                             ('mismatch', 'inode differs from the root-owned anchor')):
            with self.subTest(kind=kind):
                extra_env = {}
                if kind == 'missing':
                    self.anchor.unlink()
                elif kind == 'owner':
                    extra_env['FAKE_ANCHOR_UID'] = '2001'
                elif kind == 'mode':
                    self.anchor.chmod(0o666)
                elif kind == 'symlink':
                    self.anchor.unlink()
                    self.anchor.symlink_to(self.dir / 'absent')
                else:
                    document = json.loads(anchor)
                    document['inode'] += 1
                    self.anchor.write_text(json.dumps(document))
                before = self.anchor.read_bytes() if kind not in ('missing', 'symlink') else None
                self.assert_launch_refused(needle, extra_env=extra_env)
                self.assertEqual(self.lock.stat(), info)
                if kind == 'missing':
                    self.assertFalse(self.anchor.exists())
                elif kind == 'symlink':
                    self.assertTrue(self.anchor.is_symlink())
                    self.assertFalse((self.dir / 'absent').exists())
                else:
                    self.assertEqual(self.anchor.read_bytes(), before)
                    if kind == 'mode':
                        self.assertEqual(self.anchor.stat().st_mode & 0o777, 0o666)
                self.anchor.unlink(missing_ok=True)
                self.anchor.write_bytes(anchor)
                self.anchor.chmod(0o644)

    def test_fence_under_the_lock_refuses_before_docker_and_requires_operator_recovery(self):
        self.prepare()
        for kind in ('directory', 'file', 'symlink'):
            with self.subTest(kind=kind):
                before = len([e for e in self.events() if e[0] == 'docker'])
                result = self.run_block(self.blocks[-1], fence=kind)
                self.assertEqual(result.returncode, 1, result.stderr)
                log = (self.work / 'confirm.log').read_text()
                self.assertIn(str(self.fence), log)
                self.assertIn('operator recovery', log)
                self.assertIn('all owned resources stopped before fence cleanup', log)
                self.assertEqual(len([e for e in self.events() if e[0] == 'docker']), before)
                self.assertTrue(os.path.lexists(self.fence))
                # Remove only this test's synthetic fence, never a machine fence.
                if kind == 'directory':
                    self.fence.rmdir()
                else:
                    self.fence.unlink()

    def test_absent_fence_permits_the_detached_named_confirmation_under_the_lock(self):
        import fcntl
        self.prepare()
        result = self.run_block(self.blocks[-1])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([name for name, _ in self.events()],
                         ['id', 'docker', 'git', 'nohup', 'python3', 'git', 'git', 'git', 'docker'])
        self.assertEqual((self.work / 'confirm.log').read_text(), 'fake confirmation launched\n')
        self.assertEqual((self.dir / 'anchor-reads').read_text().splitlines(), ['true'])
        self.assertEqual(list(self.work.glob('host-authority-*')), [])
        # Docker checked exclusion in its own process; the parent releases only on return.
        with self.lock.open('r') as released:
            fcntl.flock(released, fcntl.LOCK_EX | fcntl.LOCK_NB)
        self.assertIn('https://get.nexte.st/0.9.132/linux', self.doc)
        self.assertIn('docker ps --filter name=afq-confirm', self.doc)
        self.assertIn('tail -n 20 af-quarantine-$COMMIT/confirm.log', self.doc)
        for artifact in ('run/confirm.json', 'confirm.log'):
            self.assertIn(f'scp "claw-3:af-quarantine-$COMMIT/{artifact}"', self.doc)
        self.assertIn('1 min\n35 s', self.doc)
        self.assertIn('All nine were clean on\nLinux', self.doc)

    def test_non_root_non_1000_preparation_elevates_ownership_before_any_docker(self):
        self.prepare(uid='2001')
        self.assertEqual([name for name, _ in self.events()], ['id', 'sudo', 'chown', 'docker'])
        self.assertEqual((self.work / 'owner').read_text(), '1000')
        self.assertEqual(self.run_block(self.blocks[-1], uid='2001').returncode, 0)

    def test_uid_1000_prepares_without_sudo_or_chown(self):
        self.prepare()
        self.assertEqual([name for name, _ in self.events()], ['id', 'docker'])

    def test_root_prepares_with_direct_chown_without_sudo(self):
        self.prepare(uid='0')
        self.assertEqual([name for name, _ in self.events()], ['id', 'chown', 'docker'])
        self.assertEqual((self.work / 'owner').read_text(), '1000')

    def test_busy_lock_retry_runs_only_launch_and_rechecks_checkout_before_nohup(self):
        import fcntl
        self.assertEqual(len(self.blocks), 2, 'preparation and launch must be separate SSH blocks')
        prepare, launch = self.blocks
        self.assertIn('git clone', prepare)
        self.assertNotIn('nohup', prepare)
        self.assertNotIn('git clone', launch)
        self.assertIn('WORK="$HOME/af-quarantine-$COMMIT"', prepare)
        self.assertIn('WORK="$HOME/af-quarantine-$COMMIT"', launch)
        self.assertIn('repeat only the launch block', self.doc)
        self.prepare()
        with self.lock.open('a') as held:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            result = self.run_block(launch)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual((self.work / 'confirm.log').read_text(), '')
        self.assertEqual(self.run_block(launch).returncode, 0)
        self.assertEqual([name for name, _ in self.events()],
                         ['id', 'docker', 'git', 'nohup', 'python3',
                          'git', 'git', 'git', 'git', 'nohup', 'python3',
                          'git', 'git', 'git', 'docker'])
        before = self.events()
        (self.work / 'run/src/.git/HEAD').write_text('1' * 40 + '\n')
        self.assertNotEqual(self.run_block(launch).returncode, 0)
        self.assertEqual(self.events()[len(before):], [['git', before[2][1]]])


if __name__ == '__main__':
    unittest.main()
