#!/usr/bin/env python3
"""Offline-first, marker-only host wake experiments. No subprocesses or network access."""
import argparse
import hashlib
import json
import os
import re
import stat
import sys

SCHEMA = 'af/task-wake-probe@1'
EVIDENCE = 'af/task-wake-evidence@1'
LIMIT = 2048
ROWS = 128
HOSTS = ('pi', 'codex', 'claude')
SCENARIOS = ('idle', 'busy', 'reconnect', 'restart', 'duplicate', 'ambiguous')
IDENTITIES = ('run_id', 'host', 'session_id', 'probe_id', 'delivery_id')
STAGES = ('dry_run', 'unsupported', 'unavailable', 'inbox_accepted', 'admission_intent',
          'native_returned', 'native_admitted', 'admission_unknown', 'turn_started', 'report_observed')
REASONS = ('offline', 'no_original_session_route', 'extension_required', 'host_unavailable',
           'persisted', 'before_native_call', 'no_native_receipt', 'lost_ack', 'observer_supplied')
TOKEN = r'[A-Za-z0-9_-]{1,128}'


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=True)


def closed(value, keys):
    if not isinstance(value, dict) or set(value) != set(keys):
        raise ValueError('malformed or unknown fields')


def token(value):
    if not isinstance(value, str) or not re.fullmatch(TOKEN, value):
        raise ValueError('invalid opaque identity')


def parse(raw):
    if len(raw) > LIMIT:
        raise ValueError('oversized record')
    value = json.loads(raw)
    # The writer's canonical ASCII framing rejects duplicate JSON keys as well as alternate
    # encodings. No arbitrary strings, commands, URLs or environment values are admitted.
    if raw != (canonical(value) + '\n').encode('ascii'):
        raise ValueError('noncanonical or repeated JSON keys')
    return value


def packet(value):
    closed(value, ('schema', *IDENTITIES, 'scenario', 'marker'))
    if value['schema'] != SCHEMA or value['host'] not in HOSTS or value['scenario'] not in SCENARIOS:
        raise ValueError('unsupported packet')
    for key in IDENTITIES:
        token(value[key])
    if not isinstance(value['marker'], str) or not re.fullmatch(r'AF_WAKE_[a-f0-9]{16}', value['marker']):
        raise ValueError('invalid harmless marker')
    return value


def bounded_file(path, limit):
    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
    with os.fdopen(fd, 'rb') as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ValueError('input must be a regular file')
        raw = stream.read(limit + 1)
    if len(raw) > limit:
        raise ValueError('oversized input')
    return raw


def read_packet(path, session):
    result = packet(parse(bounded_file(path, LIMIT)))
    token(session)
    if result['session_id'] != session:
        raise ValueError('wrong original session')
    return result


def digest(value):
    return 'sha256:' + hashlib.sha256(canonical(value).encode('ascii')).hexdigest()


def event(value, stage, reason, reference=None):
    return {'schema': EVIDENCE, **{k: value[k] for k in IDENTITIES},
            'payload_sha256': digest(value), 'stage': stage, 'reason': reason, 'reference': reference}


def validate_events(raw, value):
    if len(raw) > LIMIT * ROWS:
        raise ValueError('oversized evidence')
    lines = raw.splitlines(keepends=True)
    if not 1 <= len(lines) <= ROWS:
        raise ValueError('evidence row bound')
    result = []
    # Evidence is one exact delivery, not a mixed-host log. Stages are observations, never
    # authority to call an API. Repeated stages must preserve every byte of their payload.
    seen = {}
    for line in lines:
        row = parse(line)
        closed(row, ('schema', *IDENTITIES, 'payload_sha256', 'stage', 'reason', 'reference'))
        if row['schema'] != EVIDENCE or any(row[k] != value[k] for k in IDENTITIES):
            raise ValueError('wrong evidence identity/session')
        if row['payload_sha256'] != digest(value):
            raise ValueError('altered payload under delivery key')
        if row['stage'] not in STAGES or row['reason'] not in REASONS:
            raise ValueError('unknown receipt stage/reason')
        if row['reference'] is not None:
            token(row['reference'])
        stage, reason = row['stage'], row['reason']
        allowed = {
            'dry_run': ('offline',), 'unsupported': ('no_original_session_route', 'extension_required'),
            'unavailable': ('host_unavailable',), 'inbox_accepted': ('persisted',),
            'admission_intent': ('before_native_call',), 'native_returned': ('no_native_receipt',),
            'admission_unknown': ('no_native_receipt', 'lost_ack'),
            'native_admitted': ('observer_supplied',), 'turn_started': ('observer_supplied',),
            'report_observed': ('observer_supplied',)}
        if reason not in allowed[stage]:
            raise ValueError('stage/reason mismatch')
        if (reason == 'observer_supplied') != (row['reference'] is not None):
            raise ValueError('receipt reference mismatch')
        if stage in seen and row != seen[stage]:
            raise ValueError('altered receipt under repeated stage key')
        if stage not in seen:
            previous = set(seen)
            prerequisites = {'admission_intent': 'inbox_accepted',
                             'native_returned': 'admission_intent',
                             'native_admitted': 'admission_intent',
                             'admission_unknown': 'admission_intent',
                             'turn_started': 'admission_intent',
                             'report_observed': 'turn_started'}
            if stage in prerequisites and prerequisites[stage] not in previous:
                raise ValueError('receipt without prior stage')
            isolated = {'dry_run', 'unsupported', 'unavailable'}
            if previous and (stage in isolated or previous & isolated):
                raise ValueError('refusal/dry-run mixed with receipt')
        seen[stage] = row
        result.append(row)
    return result


def probe(value, session, *, live=False, available=True):
    # No live Python transport is installed. In particular thread/resume and SDK creation
    # cannot prove attachment to an already open interactive frontend. Never launch them.
    if value['session_id'] != session:
        raise ValueError('wrong original session')
    if not live:
        return event(value, 'dry_run', 'offline')
    if not available:
        return event(value, 'unavailable', 'host_unavailable')
    return event(value, 'unsupported', 'extension_required' if value['host'] == 'pi'
                 else 'no_original_session_route')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    make = commands.add_parser('packet', help='print one canonical harmless packet; no live work')
    make.add_argument('--host', choices=HOSTS, required=True)
    for name in ('run-id', 'session-id', 'probe-id', 'delivery-id', 'marker'):
        make.add_argument('--' + name, required=True)
    make.add_argument('--scenario', choices=SCENARIOS, required=True)
    for name in ('probe', 'validate', 'observe'):
        command = commands.add_parser(name)
        command.add_argument('--packet', required=True)
        command.add_argument('--session-id', required=True)
        if name == 'probe':
            command.add_argument('--live', action='store_true')
        elif name == 'validate':
            command.add_argument('--evidence', required=True)
        else:
            command.add_argument('--stage', choices=('native_admitted', 'turn_started', 'report_observed'), required=True)
            command.add_argument('--reference', required=True)
    args = parser.parse_args()
    try:
        if args.command == 'packet':
            value = packet({'schema': SCHEMA, **{k: getattr(args, k) for k in IDENTITIES},
                            'scenario': args.scenario, 'marker': args.marker})
            print(canonical(value))
        else:
            value = read_packet(args.packet, args.session_id)
            if args.command == 'probe':
                row = probe(value, args.session_id, live=args.live)
                print(canonical(row))
                return 2 if row['stage'] != 'dry_run' else 0
            if args.command == 'observe':
                token(args.reference)
                print(canonical(event(value, args.stage, 'observer_supplied', args.reference)))
                return 0
            validate_events(bounded_file(args.evidence, LIMIT * ROWS), value)
            print('structurally consistent only; not independent live proof')
        return 0
    except (ValueError, OSError, UnicodeError, RecursionError):
        print('refused: invalid, mismatched, unavailable or oversized input', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
