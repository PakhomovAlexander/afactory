#!/usr/bin/env python3
"""Independent bounded maintenance liveness observer; no model, schedule or remote writes.

Reads the native checksummed Scenario SQLite journal read-only. Alerts enter a durable
idempotent outbox before notification. The default CLI prints pending alerts (dry-run);
--ack acknowledges one exact alert only after an external scoped notifier retained it.
No PR state is used as Run acceptance or as evidence that a daily occurrence happened.
"""
import argparse
import bisect
import datetime as dt
from contextlib import closing
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import selectors
import signal
import stat
import subprocess
import time
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location('ci_health', ROOT / 'scripts/ci-health.py')
C = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(C)
DAY = 86_400_000
GRACE = 2 * 3_600_000
MAX_DAYS = 4096
MAX_ALERTS = 16384
MAX_JOURNAL = 16 * 1024 * 1024
KINDS = ('missed_occurrence', 'run_stalled', 'run_failed_before_report')


def schema_documents():
    ident = {'type': 'string', 'pattern': '^sha256:[0-9a-f]{64}$'}
    count = {'type': 'integer', 'minimum': 0, 'maximum': C.SAFE}
    config = C.obj({'schema': {'const': 'af.maintenance-watchdog-config/1'},
        'ant_id': C.text(128), 'ant_revision': ident, 'writer_version': C.text(64),
        'notification': C.text(256), 'notifier': {'anyOf': [{'type': 'null'}, C.obj({
            'executable': C.text(4096), 'digest': ident,
            'max_wall_ms': {'type': 'integer', 'minimum': 1, 'maximum': 30000},
            'max_output_bytes': {'type': 'integer', 'minimum': 1024, 'maximum': 65536}})]},
        'start_day': count,
        'hour': {'type': 'integer', 'minimum': 0, 'maximum': 23},
        'minute': {'type': 'integer', 'minimum': 0, 'maximum': 59},
        'zone': C.obj({'name': C.text(128), 'initial_offset': {'type': 'integer', 'minimum': -86400, 'maximum': 86400},
            'transitions': {'type': 'array', 'maxItems': 100000, 'items': {'type': 'array', 'minItems': 2, 'maxItems': 2,
                'items': {'type': 'integer', 'minimum': -C.SAFE, 'maximum': C.SAFE}}},
            'valid_until_unix_seconds': {'anyOf': [{'type': 'null'}, count]}}),
        'max_wall_ms': {'type': 'integer', 'minimum': 1, 'maximum': 60000},
        'grace_ms': {'const': GRACE}, 'report_step': {'const': 'report-outcome'}})
    alert = C.obj({'schema': {'const': 'af.maintenance-watchdog-alert/1'}, 'alert_id': ident,
        'ant_id': C.text(128), 'ant_revision': ident, 'expected_occurrence': ident,
        'expected_date': {'type': 'string', 'pattern': '^20[0-9]{2}-[0-9]{2}-[0-9]{2}$'},
        'kind': {'enum': list(KINDS)}, 'run_id': {'anyOf': [{'type': 'null'}, C.text(128)]},
        'notification': C.text(256), 'observed_unix_ms': count})
    return {name + '-v1.json': {'$schema': 'https://json-schema.org/draft/2020-12/schema',
        '$id': 'urn:af:schema:' + name + ':1', 'title': name.replace('-', ' ').capitalize() + ' v1', **value} for name, value in [
            ('maintenance-watchdog-config', config), ('maintenance-watchdog-alert', alert)]}


_VALIDATOR = None


def validate(value, name):
    global _VALIDATOR
    if _VALIDATOR is None:
        spec = importlib.util.spec_from_file_location('watchdog_schema', ROOT / 'scripts/backlog-check-triage.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        _VALIDATOR = module.Validator()
        for schema in schema_documents().values():
            _VALIDATOR.schemas[schema['$id']] = schema
    errors = _VALIDATOR.errors(value, 'urn:af:schema:' + name + ':1')
    C.require(not errors, '; '.join(errors[:4]))
    C.require(len(C.canonical(value)) <= MAX_JOURNAL, 'Oversized watchdog document')
    return value


def read_world(path, writer_version):
    C.require(not path.is_symlink(), 'Journal symlink refused')
    # Read-only connection neither creates a missing journal nor creates a second scheduler.
    with closing(sqlite3.connect(path.resolve().as_uri() + '?mode=ro', uri=True, timeout=5)) as db:
        db.execute('PRAGMA query_only=ON')
        db.set_progress_handler(lambda: 1, 100000)
        row = db.execute('SELECT length(CAST(document AS BLOB)), digest FROM journal WHERE id=1').fetchone()
        C.require(row is not None and row[0] <= MAX_JOURNAL, 'Missing or oversized native journal')
        raw, expected = db.execute('SELECT document,digest FROM journal WHERE id=1').fetchone()
    C.require('sha256:' + hashlib.sha256(raw.encode()).hexdigest() == expected, 'Native journal checksum mismatch')
    world = C.loads(raw)
    C.require(set(world) == {'schema', 'writer_version', 'ants', 'runs', 'roadmaps'} and
              world['schema'] == 'af/scenario-store@7' and world['writer_version'] == writer_version,
              'Unsupported native journal version')
    C.require(isinstance(world['ants'], dict) and isinstance(world['runs'], dict), 'Malformed native journal')
    C.require(len(world['ants']) <= 4096 and len(world['runs']) <= 16384, 'Native receipt count bound exhausted')
    return world


def due(zone, day, hour, minute, prepared=None):
    transitions = zone['transitions']
    C.require(len(transitions) <= 100000 and all(len(t) == 2 for t in transitions)
              and all(a[0] < b[0] for a, b in zip(transitions, transitions[1:])), 'Malformed pinned zone')
    offsets, times = prepared if prepared is not None else (
        {zone['initial_offset'], *(offset for _, offset in transitions)}, [t for t, _ in transitions])
    C.require(len(offsets) <= 256 and all(abs(o) <= 86400 for o in offsets), 'Pinned offset out of bounds')
    local = day * 86400 + hour * 3600 + minute * 60
    def offset(at):
        C.require(zone['valid_until_unix_seconds'] is None or at < zone['valid_until_unix_seconds'], 'Pinned timezone horizon exhausted')
        index = bisect.bisect_right(times, at)
        return transitions[index - 1][1] if index else zone['initial_offset']
    matches = [local - o for o in offsets if offset(local - o) == o]
    if matches:
        return min(matches) * 1000
    before = zone['initial_offset']
    for at, after in transitions:
        if after > before and at + before <= local < at + after:
            return at * 1000
        before = after
    raise ValueError('Unresolvable pinned expected occurrence')


def findings(config, world, now, deadline=None):
    validate(config, 'maintenance-watchdog-config')
    deadline = deadline if deadline is not None else time.monotonic() + config['max_wall_ms'] / 1000
    C.require(type(now) is int and 0 < now <= C.SAFE, 'Invalid watchdog clock')
    ant = world['ants'].get(config['ant_id'])
    if ant is not None:
        pin = ant['pin']
        C.require(ant['schema'] == 'af/ant-state@6' and pin['revision'] == config['ant_revision']
            and pin['definition']['id'] == config['ant_id'] and pin['zone'] == config['zone']
            and pin['definition']['schedule']['hour'] == config['hour']
            and pin['definition']['schedule']['minute'] == config['minute'], 'Watchdog expected Ant pin changed')
        C.require(pin['schema'] == 'af/ant-pin@6'
            and pin['revision'] == C.digest([pin['definition'], pin['scenario'], pin['zone']])
            and pin['scenario']['schema'] == 'af/scenario-pin@6'
            and pin['scenario']['revision'] == C.digest([pin['scenario']['bundle'], pin['scenario']['policy']]),
            'Corrupt pinned Ant/Scenario identity')
        changes = ant['changes']
        C.require(len(changes) <= 16384 and all(type(t) is int and t <= now and mode in ('enabled', 'paused', 'disabled') for t, mode in changes)
                  and all(a[0] <= b[0] for a, b in zip(changes, changes[1:])), 'Malformed durable Ant mode history')
        occurrences = ant['occurrences']
        C.require(len(occurrences) <= MAX_DAYS and len({o['local_day'] for o in occurrences}) == len(occurrences), 'Malformed occurrence history')
    else:
        changes, occurrences = [], []
    receipts = {o['local_day']: o for o in occurrences}
    # UTC date plus a day covers every permitted pinned offset. due() decides eligibility.
    last = now // DAY + 1
    if last < config['start_day']:
        return []
    C.require(last - config['start_day'] <= MAX_DAYS, 'Expected occurrence scan bound exhausted')
    alerts = []
    zone = config['zone']
    prepared_zone = ({zone['initial_offset'], *(o for _, o in zone['transitions'])}, [t for t, _ in zone['transitions']])
    C.require(len(prepared_zone[0]) <= 256, 'Pinned zone type bound exhausted')
    for day in range(config['start_day'], last + 1):
        C.require(time.monotonic() < deadline, 'Watchdog observation wall bound exhausted')
        at = due(config['zone'], day, config['hour'], config['minute'], prepared_zone)
        if now < at:
            continue
        # A visible pause/disable suppresses only occurrences due at/after that durable change.
        mode = next((mode for changed, mode in reversed(changes) if changed <= at), 'enabled')
        if mode != 'enabled':
            continue
        occurrence = C.digest([config['ant_id'], config['ant_revision'], day])
        receipt = receipts.get(day)
        kind, run_id = None, None
        if receipt is not None:
            C.require(receipt['id'] == occurrence and receipt['due_unix_ms'] == at, 'Stale occurrence receipt')
            outcome = receipt['outcome']
            if outcome['kind'] in ('skipped_busy', 'skipped_missed', 'skipped_paused', 'skipped_disabled', 'skipped_budget', 'skipped_missing_time'):
                continue
            if outcome['kind'] == 'skipped_roadmap' and isinstance(outcome.get('reason'), str) and outcome['reason'].strip():
                continue
            if outcome['kind'] == 'started':
                run_id = outcome['run_id']
                run = world['runs'].get(run_id)
                if run is None:
                    if now < at + ant['pin']['definition']['execution']['per_run']['max_wall_ms']:
                        continue
                    kind = 'run_stalled'
                else:
                    C.require(run['schema'] == 'af/scenario-run@6' and run['id'] == run_id
                        and run['ant'] == config['ant_id'] and run['pin'] == ant['pin']['scenario'], 'Unrelated admitted Run')
                    C.require(at <= run['created_unix_ms'] <= run['updated_unix_ms'] <= now,
                              'Invalid admitted Run interval')
                    C.require(run['limits'] == ant['pin']['definition']['execution']['per_run'], 'Run wall/budget pin differs')
                    C.require(run['status'] in ('ready', 'running', 'waiting', 'succeeded', 'failed', 'cancelled'), 'Unknown Run status')
                    terminal = run['status'] in ('succeeded', 'failed', 'cancelled')
                    report = next((p for p in run['frames']['root']['steps'] if p['step'] == config['report_step']), None)
                    output = report.get('outputs', {}).get('result', {}) if report else {}
                    action = output.get('value', {}) if output.get('type') == 'action' else {}
                    intents = report.get('intents', []) if report else []
                    reported = (report is not None and report['status'] == 'succeeded' and report.get('skipped') is None
                        and action.get('operation') == 'notify' and bool(action.get('evidence'))
                        and action.get('destination') == ant['pin']['definition']['notification']
                        and any(i['id'] == action.get('intent_id') and i['phase'] == 'done' for i in intents))
                    if run['status'] == 'failed' and not reported:
                        kind = 'run_failed_before_report'
                    elif not terminal and now >= run['created_unix_ms'] + run['limits']['max_wall_ms']:
                        kind = 'run_stalled'
                if kind is None:
                    continue
        if kind is None:
            if now < at + config['grace_ms']:
                continue
            kind = 'missed_occurrence'
        alert = {'schema': 'af.maintenance-watchdog-alert/1',
            'alert_id': C.digest([config['ant_id'], config['ant_revision'], occurrence, kind]),
            'ant_id': config['ant_id'], 'ant_revision': config['ant_revision'],
            'expected_occurrence': occurrence,
            'expected_date': (dt.date(1970, 1, 1) + dt.timedelta(days=day)).isoformat(),
            'kind': kind, 'run_id': run_id, 'notification': config['notification'], 'observed_unix_ms': now}
        alerts.append(validate(alert, 'maintenance-watchdog-alert'))
    return alerts


class Outbox:
    """At-least-once delivery with one durable alert identity, never lost on notifier failure."""
    def __init__(self, path):
        C.require(not Path(path).is_symlink(), 'Watchdog state symlink refused')
        self.db = sqlite3.connect(path, timeout=5)
        os.chmod(path, 0o600)
        self.db.execute('PRAGMA synchronous=FULL')
        C.require(self.db.execute('PRAGMA page_size').fetchone()[0] == 4096
                  and self.db.execute('PRAGMA max_page_count=4096').fetchone()[0] <= 4096, 'Watchdog state byte bound exhausted')
        self.db.execute('CREATE TABLE IF NOT EXISTS metadata (schema TEXT NOT NULL)')
        if not self.db.execute('SELECT schema FROM metadata').fetchall():
            self.db.execute('INSERT INTO metadata VALUES (?)', ('af.maintenance-watchdog-state/1',))
        C.require(self.db.execute('SELECT schema FROM metadata').fetchall() == [('af.maintenance-watchdog-state/1',)], 'Unsupported watchdog state')
        self.db.execute('CREATE TABLE IF NOT EXISTS alerts (id TEXT PRIMARY KEY, document TEXT NOT NULL, delivered INTEGER NOT NULL CHECK(delivered IN (0,1)))')
        self.db.commit()

    def close(self):
        self.db.close()

    def stage(self, alerts):
        with self.db:
            self.db.execute('BEGIN IMMEDIATE')
            count = self.db.execute('SELECT count(*) FROM alerts').fetchone()[0]
            for alert in alerts:
                validate(alert, 'maintenance-watchdog-alert')
                if not self.db.execute('SELECT 1 FROM alerts WHERE id=?', (alert['alert_id'],)).fetchone():
                    C.require(count < MAX_ALERTS, 'Watchdog deduplication bound exhausted')
                    self.db.execute('INSERT INTO alerts VALUES (?,?,0)', (alert['alert_id'], C.canonical(alert).decode()))
                    count += 1

    def pending(self):
        rows = self.db.execute('SELECT document FROM alerts WHERE delivered=0 ORDER BY id LIMIT 64').fetchall()
        return [C.loads(raw) for (raw,) in rows]

    def acknowledge(self, alert_id):
        with self.db:
            cursor = self.db.execute('UPDATE alerts SET delivered=1 WHERE id=?', (alert_id,))
            C.require(cursor.rowcount == 1, 'Unknown alert acknowledgement')


def private(path, directory=False, owner_only=False):
    """Explicit owner-controlled installation; never follow a changed path or symlink."""
    C.require(path.is_absolute() and path.resolve() == path, 'Notifier path must be canonical')
    for ancestor in path.parents:
        try:
            (ancestor / '.git').lstat()
        except FileNotFoundError:
            continue
        raise ValueError('Notifier authority must be outside every Git checkout')
    for ancestor in [path, *path.parents]:
        info = ancestor.lstat()
        C.require(not stat.S_ISLNK(info.st_mode), 'Notifier symlink refused')
        if ancestor == path:
            C.require(info.st_uid in (0, os.geteuid()), 'Notifier owner differs')
        # A sticky root-owned /tmp is an allowed installation ancestor, never the leaf.
        writable = info.st_mode & (0o077 if ancestor == path and owner_only else 0o022)
        C.require(not writable or (ancestor != path and info.st_mode & stat.S_ISVTX),
                  'Notifier installation is writable by another account')
    C.require(path.is_dir() if directory else path.is_file(), 'Notifier path type differs')


def call_notifier(grant, request):
    executable = Path(grant['executable'])
    private(executable)
    with executable.open('rb') as stream:
        program = stream.read(1_048_577)
    C.require(len(program) <= 1_048_576 and 'sha256:' + hashlib.sha256(program).hexdigest() == grant['digest'],
              'Pinned notification adapter bytes changed')
    raw = C.canonical(request)
    C.require(len(raw) <= 4096, 'Watchdog notification input bound exceeded')
    env = {'PATH': '/usr/bin:/bin'}
    for name in ('HOME', 'XDG_CONFIG_HOME'):
        if name in os.environ:
            env[name] = os.environ[name]
    deadline = time.monotonic() + grant['max_wall_ms'] / 1000
    with subprocess.Popen([str(executable)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.DEVNULL, env=env, cwd=executable.parent,
                          start_new_session=True) as process:
        try:
            process.stdin.write(raw)
            process.stdin.close()
            response = bytearray()
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                while selector.get_map():
                    remaining = deadline - time.monotonic()
                    C.require(remaining > 0, 'Notification outcome uncertain: deadline')
                    for key, _ in selector.select(remaining):
                        data = os.read(key.fd, 65536)
                        if not data:
                            selector.unregister(key.fd)
                        response.extend(data)
                        C.require(len(response) <= grant['max_output_bytes'], 'Notification outcome uncertain: output bound')
            try:
                code = process.wait(timeout=max(0.001, deadline - time.monotonic()))
            except subprocess.TimeoutExpired as exc:
                raise ValueError('Notification outcome uncertain: deadline') from exc
            C.require(code == 0, 'Notification outcome uncertain: adapter exit')
            value = C.loads(response)
            C.require(set(value) == {'schema', 'intent_id', 'input_digest', 'outcome', 'evidence', 'observation'}
                and value['schema'] == 'af/scenario-adapter-response@1'
                and value['intent_id'] == request['intent_id'] and value['input_digest'] == request['input_digest']
                and value['outcome'] in ('absent', 'pending', 'done', 'failed')
                and value['observation'] is None and isinstance(value['evidence'], str)
                and 0 < len(value['evidence']) <= 65536, 'Malformed notification receipt')
            return value['outcome']
        finally:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass


def deliver(outbox, config, deadline=None):
    C.require(config['notifier'] is not None, 'Explicit pinned notification grant required')
    # One service call has a bounded wall; at most 64 alerts and two calls per alert.
    # All uncertain receipts remain pending. Every resume reconciles the same exact intent.
    deadline = deadline if deadline is not None else time.monotonic() + config['max_wall_ms'] / 1000
    for alert in outbox.pending():
        C.require(time.monotonic() < deadline, 'Watchdog notification wall bound exhausted')
        grant = dict(config['notifier'])
        grant['max_wall_ms'] = max(1, min(grant['max_wall_ms'], int((deadline - time.monotonic()) * 1000)))
        C.require(alert['notification'] == config['notification'] and alert['ant_id'] == config['ant_id']
                  and alert['ant_revision'] == config['ant_revision'], 'Pending alert authority changed')
        inputs = {key: {'type': 'text', 'value': value} for key, value in {
            'subject': alert['ant_id'], 'revision': alert['ant_revision'],
            'body': C.canonical(alert).decode()}.items()}
        request = {'schema': 'af/scenario-adapter-request@4', 'method': 'reconcile',
            'intent_id': alert['alert_id'], 'input_digest': C.digest(inputs),
            'operation': 'notify', 'destination': config['notification'], 'inputs': inputs}
        outcome = call_notifier(grant, request)
        if outcome == 'absent':
            C.require(time.monotonic() < deadline, 'Watchdog notification wall bound exhausted')
            grant['max_wall_ms'] = max(1, min(grant['max_wall_ms'], int((deadline - time.monotonic()) * 1000)))
            request['method'] = 'execute'
            outcome = call_notifier(grant, request)
        if outcome == 'done':
            outbox.acknowledge(alert['alert_id'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', type=Path)
    parser.add_argument('--journal', type=Path)
    parser.add_argument('--state', type=Path)
    parser.add_argument('--now-unix-ms', type=int)
    parser.add_argument('--ack')
    parser.add_argument('--notify', action='store_true')
    parser.add_argument('--schemas', action='store_true')
    parser.add_argument('--write', action='store_true')
    args = parser.parse_args()
    if args.schemas:
        for name, schema in schema_documents().items():
            path = ROOT / 'schemas' / name
            rendered = json.dumps(schema, indent=2, sort_keys=True) + '\n'
            if args.write:
                path.write_text(rendered)
            else:
                C.require(path.read_text() == rendered, 'Watchdog schema drift: ' + name)
        return
    C.require(args.config is not None and args.journal is not None and args.state is not None,
              'Watchdog requires explicit config, native journal and private state')
    if args.notify:
        private(args.config, owner_only=True)
    with args.config.open('rb') as stream:
        raw = stream.read(MAX_JOURNAL + 1)
    C.require(len(raw) <= MAX_JOURNAL, 'Watchdog config bound exceeded')
    config = validate(C.loads(raw), 'maintenance-watchdog-config')
    now = args.now_unix_ms if args.now_unix_ms is not None else int(dt.datetime.now(dt.timezone.utc).timestamp() * 1000)
    deadline = time.monotonic() + config['max_wall_ms'] / 1000
    alerts = findings(config, read_world(args.journal, config['writer_version']), now, deadline)
    C.require(time.monotonic() < deadline, 'Watchdog observation wall bound exhausted')
    outbox = Outbox(args.state)
    try:
        outbox.stage(alerts)
        if args.ack:
            outbox.acknowledge(args.ack)
        if args.notify:
            deliver(outbox, config, deadline)
        for alert in outbox.pending():
            print(C.canonical(alert).decode())
    finally:
        outbox.close()


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, TypeError, OSError, sqlite3.Error) as exc:
        print(str(exc), file=sys.stderr)
        raise SystemExit(1)
