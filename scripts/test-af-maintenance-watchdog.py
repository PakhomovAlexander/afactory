#!/usr/bin/env python3
"""Hermetic independent-watchdog service and durable liveness evidence fixtures."""
import copy
from contextlib import closing
import hashlib
from functools import lru_cache
import importlib.util
import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location('watchdog', ROOT / 'scripts/af-maintenance-watchdog.py')
W = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(W)
C = W.C
DAY = W.DAY
START = 1794182400000  # 2026-11-09 UTC
DUE = START + 4 * 3600000


@lru_cache(maxsize=1)
def ant_pin():
    bundle = json.loads((ROOT / 'fixtures/scenarios/maintenance-ci-health/scenario.json').read_text())
    policy = {'schema': 'af/scenario-policy@5', 'adapters': {}, 'task_plans': [], 'task_templates': [],
              'decision_keys': {}, 'file_roots': []}
    scenario = {'schema': 'af/scenario-pin@6', 'bundle': bundle, 'policy': policy,
                'definitions': {key: C.digest(value) for key, value in bundle['definitions'].items()},
                'revision': C.digest([bundle, policy])}
    zone = {'name': 'UTC', 'initial_offset': 0, 'transitions': [], 'valid_until_unix_seconds': None}
    definition = {'schema': 'af/ant@6', 'id': 'maintenance-ci-health', 'scenario': bundle, 'inputs': json.loads((ROOT / 'fixtures/scenarios/maintenance-ci-health/inputs.json').read_text()),
        'schedule': {'timezone': 'UTC', 'hour': 4, 'minute': 0, 'start_unix_ms': START},
        'owner': 'owner', 'notification': 'fixture:owner', 'execution': {'overlap': 'skip', 'missed': 'skip',
            'lateness_ms': 1000, 'per_run': {'max_tokens': 2000, 'max_wall_ms': 2 * DAY, 'max_dispatches': 20},
            'rolling_tokens': 2000, 'rolling_window_ms': DAY}}
    return {'schema': 'af/ant-pin@6', 'definition': definition, 'scenario': scenario, 'zone': zone,
            'revision': C.digest([definition, scenario, zone])}


def config():
    return {'schema': 'af.maintenance-watchdog-config/1', 'ant_id': 'maintenance-ci-health',
        'ant_revision': ant_pin()['revision'], 'writer_version': 'fixture-release',
        'notification': 'fixture:owner', 'notifier': None, 'start_day': START // DAY,
        'hour': 4, 'minute': 0, 'zone': {'name': 'UTC', 'initial_offset': 0,
            'transitions': [], 'valid_until_unix_seconds': None},
        'max_wall_ms': 60000, 'grace_ms': W.GRACE, 'report_step': 'report-outcome'}


def world():
    cfg = config()
    # Native receipt fields are kept in their native shapes; the Rust domain test below
    # additionally exercises real serialized Ants/Runs and the real SQLite reader.
    pin = copy.deepcopy(ant_pin())
    return {'schema': 'af/scenario-store@7', 'writer_version': cfg['writer_version'], 'roadmaps': {},
        'runs': {}, 'ants': {cfg['ant_id']: {'schema': 'af/ant-state@6', 'pin': pin,
            'state': 'enabled', 'changes': [[START, 'enabled']], 'occurrences': []}}}


def occurrence(day=0, kind='started', **extra):
    cfg = config()
    local = cfg['start_day'] + day
    identity = C.digest([cfg['ant_id'], cfg['ant_revision'], local])
    outcome = {'kind': kind, **extra}
    if kind == 'started':
        outcome.update(run_id='ant-' + identity[7:], reserved_tokens=2000)
    return {'id': identity, 'local_day': local, 'due_unix_ms': DUE + day * DAY, 'outcome': outcome}


def admitted(value, status='waiting', reported=True):
    receipt = occurrence()
    ant = value['ants'][config()['ant_id']]
    ant['occurrences'].append(receipt)
    run_id = receipt['outcome']['run_id']
    value['runs'][run_id] = {'schema': 'af/scenario-run@6', 'id': run_id, 'ant': config()['ant_id'],
        'pin': copy.deepcopy(ant['pin']['scenario']), 'status': status, 'created_unix_ms': DUE,
        'updated_unix_ms': DUE, 'limits': copy.deepcopy(ant['pin']['definition']['execution']['per_run']),
        'frames': {'root': {'steps': [{'step': 'report-outcome', 'status': 'succeeded', 'skipped': None,
            'intents': [{'id': C.digest('report intent'), 'phase': 'done'}],
            'outputs': {'result': {'type': 'action', 'value': {'evidence': 'durable report notification',
                'operation': 'notify', 'destination': 'fixture:owner', 'intent_id': C.digest('report intent')}}}}] if reported else []}}}
    return value['runs'][run_id]


def journal(path, value):
    raw = C.canonical(value).decode()
    with closing(sqlite3.connect(path)) as db, db:
        db.execute('CREATE TABLE IF NOT EXISTS journal (id INTEGER PRIMARY KEY CHECK(id=1),document TEXT,digest TEXT)')
        db.execute('INSERT OR REPLACE INTO journal VALUES (1,?,?)', (raw, 'sha256:' + hashlib.sha256(raw.encode()).hexdigest()))


class WatchdogTests(unittest.TestCase):
    def test_future_expected_start_has_no_due_occurrence(self):
        cfg = config()
        cfg['start_day'] += 5
        value = world()
        value['ants'] = {}
        self.assertEqual(W.findings(cfg, value, DUE), [])

    def test_no_tick_after_two_hour_grace_and_one_durable_alert_across_restart(self):
        cfg, value = config(), world()
        self.assertEqual(W.findings(cfg, value, DUE + W.GRACE - 1), [])
        alerts = W.findings(cfg, value, DUE + W.GRACE)
        self.assertEqual([a['kind'] for a in alerts], ['missed_occurrence'])
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'outbox.sqlite'
            outbox = W.Outbox(path)
            outbox.stage(alerts)
            outbox.close()
            outbox = W.Outbox(path)
            outbox.stage(W.findings(cfg, value, DUE + W.GRACE + 1))
            self.assertEqual(outbox.pending(), alerts)
            outbox.acknowledge(alerts[0]['alert_id'])
            outbox.stage(alerts)
            self.assertEqual(outbox.pending(), [])
            self.assertEqual(outbox.db.execute('SELECT count(*) FROM alerts').fetchone()[0], 1)
            outbox.close()

    def test_skipped_admission_with_and_without_durable_reason(self):
        value = world()
        ant = value['ants'][config()['ant_id']]
        for kind, extra in [('skipped_busy', {}), ('skipped_budget', {}), ('skipped_missed', {}),
                            ('skipped_roadmap', {'reason': 'No eligible complete goal fits all roadmap budgets'})]:
            ant['occurrences'] = [occurrence(kind=kind, **extra)]
            self.assertEqual(W.findings(config(), value, DUE + W.GRACE), [])
        ant['occurrences'] = [occurrence(kind='skipped_roadmap', reason='')]
        self.assertEqual(W.findings(config(), value, DUE + W.GRACE)[0]['kind'], 'missed_occurrence')
        ant['occurrences'] = []  # A process-memory skip without a committed receipt proves nothing.
        self.assertEqual(W.findings(config(), value, DUE + W.GRACE)[0]['kind'], 'missed_occurrence')

    def test_failure_before_report_is_independent_of_scenario_completion(self):
        value = world()
        run = admitted(value, 'failed', False)
        self.assertEqual(W.findings(config(), value, DUE + 1)[0]['kind'], 'run_failed_before_report')
        run['frames']['root']['steps'] = [{'step': 'report-outcome', 'status': 'succeeded',
            'skipped': 'condition false', 'outputs': {}}]
        self.assertEqual(W.findings(config(), value, DUE + 2)[0]['kind'], 'run_failed_before_report')
        run['frames']['root']['steps'] = [{'step': 'report-outcome', 'status': 'succeeded',
            'skipped': None, 'intents': [{'id': C.digest('report intent'), 'phase': 'done'}],
            'outputs': {'result': {'type': 'action', 'value': {'evidence': 'retained notification',
                'operation': 'notify', 'destination': 'fixture:owner', 'intent_id': C.digest('report intent')}}}}]
        self.assertEqual(W.findings(config(), value, DUE + 3), [])

    def test_open_pr_across_next_heartbeat_does_not_delay_the_observer_or_duplicate_alerts(self):
        value = world()
        admitted(value, 'waiting', True)
        ant = value['ants'][config()['ant_id']]
        ant['occurrences'].append(occurrence(day=1, kind='skipped_busy'))
        self.assertEqual(W.findings(config(), value, DUE + DAY + W.GRACE), [])
        # A waiting PR is still active and eventually exhausts exactly its pinned wall.
        ant['occurrences'].append(occurrence(day=2, kind='skipped_busy'))
        alerts = W.findings(config(), value, DUE + 2 * DAY)
        self.assertEqual([a['kind'] for a in alerts], ['run_stalled'])
        self.assertEqual(W.findings(config(), value, DUE + 2 * DAY + 1)[0]['alert_id'], alerts[0]['alert_id'])

    def test_pause_and_disable_do_not_erase_an_already_due_alert(self):
        for mode in ('paused', 'disabled'):
            value = world()
            ant = value['ants'][config()['ant_id']]
            ant['changes'].append([DUE + 1, mode])
            ant['state'] = mode
            alerts = W.findings(config(), value, DUE + DAY + W.GRACE)
            self.assertEqual([a['expected_date'] for a in alerts], ['2026-11-09'])
            ant['changes'][-1][0] = DUE
            self.assertEqual(W.findings(config(), value, DUE + DAY + W.GRACE), [])

    def test_native_journal_checksum_versions_and_receipt_bounds_fail_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'native.sqlite'
            value = world()
            journal(path, value)
            self.assertEqual(W.read_world(path, config()['writer_version']), value)
            with closing(sqlite3.connect(path)) as db, db:
                db.execute("UPDATE journal SET digest='forged'")
            with self.assertRaisesRegex(ValueError, 'checksum'):
                W.read_world(path, config()['writer_version'])
            journal(path, value)
            with self.assertRaisesRegex(ValueError, 'version'):
                W.read_world(path, 'another-release')
        with self.assertRaisesRegex(ValueError, 'scan bound'):
            W.findings(config(), world(), DUE + 4097 * DAY)
        value = world()
        value['ants'][config()['ant_id']]['occurrences'] = [occurrence(), occurrence()]
        with self.assertRaisesRegex(ValueError, 'history'):
            W.findings(config(), value, DUE + W.GRACE)

    def test_dry_run_cli_reads_real_sqlite_and_retains_one_notification(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'config.json').write_text(json.dumps(config()))
            journal(root / 'native.sqlite', world())
            command = [sys.executable, '-I', str(ROOT / 'scripts/af-maintenance-watchdog.py'),
                '--config', str(root / 'config.json'), '--journal', str(root / 'native.sqlite'),
                '--state', str(root / 'watchdog.sqlite'), '--now-unix-ms', str(DUE + W.GRACE)]
            first = subprocess.run(command, text=True, capture_output=True, check=True)
            second = subprocess.run(command, text=True, capture_output=True, check=True)
            self.assertEqual(first.stdout, second.stdout)
            alert = json.loads(first.stdout)
            self.assertEqual(alert['kind'], 'missed_occurrence')
            acknowledged = subprocess.run(command + ['--ack', alert['alert_id']], text=True, capture_output=True, check=True)
            self.assertEqual(acknowledged.stdout, '')

    def test_scoped_notification_reconciles_a_lost_response_without_duplicate_delivery(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            root.chmod(0o700)
            executable = root / 'notifier'
            executable.write_bytes((ROOT / 'fixtures/scenarios/maintenance-ci-health/watchdog-notifier.py').read_bytes())
            executable.chmod(0o700)
            cfg = config()
            cfg['notifier'] = {'executable': str(executable),
                'digest': 'sha256:' + hashlib.sha256(executable.read_bytes()).hexdigest(),
                'max_wall_ms': 10000, 'max_output_bytes': 4096}
            outbox = W.Outbox(root / 'watchdog.sqlite')
            outbox.stage(W.findings(cfg, world(), DUE + W.GRACE))
            (root / 'lose-response').touch()
            with self.assertRaisesRegex(ValueError, 'uncertain'):
                W.deliver(outbox, cfg)
            self.assertEqual(len(outbox.pending()), 1)
            outbox.close()
            outbox = W.Outbox(root / 'watchdog.sqlite')
            W.deliver(outbox, cfg)
            self.assertEqual(outbox.pending(), [])
            self.assertEqual(len((root / 'effects').read_text().splitlines()), 1)
            outbox.close()
            executable.write_text('changed executable')
            with self.assertRaisesRegex(ValueError, 'bytes changed'):
                W.call_notifier(cfg['notifier'], {})

    def test_versioned_schema_parity(self):
        for name, value in W.schema_documents().items():
            self.assertEqual(json.loads((ROOT / 'schemas' / name).read_text()), value)


if __name__ == '__main__':
    unittest.main()
