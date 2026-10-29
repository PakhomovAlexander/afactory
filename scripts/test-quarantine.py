#!/usr/bin/env python3
"""Propose test quarantine entries from af Tasks' gate flaky lists (ADR-0264; stdlib only).

    scripts/test-quarantine.py scan --list FILE | --state DIR [...] [--inventory FILE]
                                    [--today YYYY-MM-DD] [--write] [--json]

`scan` reads `af task list --json` documents (`af/task-list@3` or `@4`), resolves each row's
`gate.flaky` names against the default-feature nextest inventory and proposes, as `quarantined`,
every identity flaky in at least 2 distinct Tasks updated within the 7 days ending at the end of
today (UTC). Without --write it changes nothing; --write adds the proposals to
.config/test-quarantine.json through scripts/test-classes.py's guards, or writes nothing.
"""
import argparse
import datetime as dt
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location('test_classes', ROOT / 'scripts/test-classes.py')
tc = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tc)

LISTS = ('af/task-list@3', 'af/task-list@4')
SCAN = 'af.test-quarantine-scan/1'
MIN_TASKS = 2
WINDOW_DAYS = 7
MAX_EVIDENCE = 10
NOT_A_TEST = 'not a test'


def parse_today(value):
    if value is None:
        return tc.quarantine_today()
    try:
        if re.fullmatch(r'\d{4}-\d{2}-\d{2}', value):
            return dt.date.fromisoformat(value)
    except ValueError:
        pass
    raise ValueError(f'--today {value!r} is not a YYYY-MM-DD date')


def list_rows(document, where):
    """The (task_id, updated_unix_ms, flaky names) of one task-list document's rows."""
    if not isinstance(document, dict) or document.get('schema') not in LISTS:
        schema = document.get('schema') if isinstance(document, dict) else None
        raise ValueError(f'{where}: unsupported task list schema {schema!r}; '
                         f'expected one of {", ".join(LISTS)}')
    tasks = document.get('tasks')
    if not isinstance(tasks, list):
        raise ValueError(f'{where}: `tasks` is not a list')
    rows = []
    for row in tasks:
        task_id = row.get('task_id') if isinstance(row, dict) else None
        updated = row.get('updated_unix_ms') if isinstance(row, dict) else None
        if not isinstance(task_id, str) or not task_id or type(updated) is not int or updated < 0:
            raise ValueError(f'{where}: a task row without a `task_id` and `updated_unix_ms`')
        gate = row.get('gate')
        if gate is None:
            continue  # No check recorded a receipt: no evidence.
        flaky = gate.get('flaky') if isinstance(gate, dict) else None
        if not isinstance(flaky, list) or not all(isinstance(name, str) for name in flaky):
            raise ValueError(f'{where}: {task_id}: `gate.flaky` is not a list of test names')
        rows.append((task_id, updated, flaky))
    return rows


def read_lists(files, states, af):
    documents = []
    for path in files:
        try:
            documents.append((json.loads(Path(path).read_text()), path))
        except json.JSONDecodeError as error:
            raise ValueError(f'{path}: not JSON: {error}') from None
    for state in states:
        result = subprocess.run([af, 'task', 'list', '--json', '--state', state],
                                capture_output=True, text=True)
        if result.returncode != 0:
            raise ValueError(f'{af} task list --json --state {state}: exit {result.returncode}: '
                             f'{result.stderr.strip()}')
        try:
            documents.append((json.loads(result.stdout), f'--state {state}'))
        except json.JSONDecodeError as error:
            raise ValueError(f'--state {state}: af printed no JSON: {error}') from None
    return [row for document, where in documents for row in list_rows(document, where)]


def default_inventory():
    """The default-feature nextest list, built as test-classes.py's build_inventory builds it."""
    result = subprocess.run(['cargo', 'nextest', 'list', '--workspace', '--locked',
                             '--message-format', 'json'], stdout=subprocess.PIPE, check=True, cwd=ROOT)
    return json.loads(result.stdout)


def resolver(inventory, cat):
    """name -> (identity, None) or (None, reason) over the audited default-feature inventory."""
    entries = tc.partition(inventory, cat, {})
    identities = {e['binary'] + ' ' + e['test']: e for e in entries}
    by_name = {}
    for identity, entry in identities.items():
        by_name.setdefault(entry['test'], []).append(identity)

    def admitted(identity):
        binary, name = identity.split(' ', 1)
        if identities[identity]['ignored']:
            return False
        return not any(binary == selected and pattern in name
                       for selected, pattern in tc.FEATURE_SELECTORS.values())

    def resolve(name):
        if name in identities:
            found = [name]
        else:
            found = sorted(by_name.get(name, []))
        if len(found) > 1:
            return None, 'ambiguous: ' + ', '.join(found)
        if not found or not admitted(found[0]):
            return None, NOT_A_TEST
        return found[0], None
    return resolve


def scan(rows, inventory, current, today, cat=None):
    """The scan's facts: proposals within the cap, the over-cap identities and rejected names."""
    cat = cat or tc.catalog()
    resolve = resolver(inventory, cat)
    end = dt.datetime.combine(today + dt.timedelta(days=1), dt.time(), dt.timezone.utc)
    end_ms = int(end.timestamp() * 1000)
    start_ms = end_ms - WINDOW_DAYS * 86_400_000
    tasks, rejected = {}, {}
    for task_id, updated, flaky in rows:
        if not start_ms <= updated < end_ms:
            continue  # Outside the window: neither evidence nor a rejected name.
        for name in flaky:
            identity, reason = resolve(name)
            if identity is None:
                rejected[name] = reason
            else:
                tasks.setdefault(identity, set()).add(task_id)
    held = current['tests']
    ranked = sorted(((identity, sorted(ids)) for identity, ids in tasks.items()
                     if len(ids) >= MIN_TASKS and identity not in held),
                    key=lambda item: (-len(item[1]), item[0]))
    room = max(tc.QUARANTINE_LIMIT - len(held), 0)
    since, expires = today.isoformat(), (today + dt.timedelta(days=tc.QUARANTINE_DAYS)).isoformat()
    proposed = [{'identity': identity, 'tasks': len(ids),
                 'entry': {'state': 'quarantined', 'since': since, 'expires': expires,
                           'evidence': ids[:MAX_EVIDENCE]}}
                for identity, ids in ranked[:room]]
    return {'schema': SCAN, 'today': since,
            'window': {'start_unix_ms': start_ms, 'end_unix_ms': end_ms},
            'proposed': proposed, 'over_cap': [identity for identity, _ in ranked[room:]],
            'held': sorted(held),
            'rejected': [{'name': name, 'reason': rejected[name]} for name in sorted(rejected)]}


def written(current, facts, inventory, today, cat):
    """The quarantine file with the proposals added, after every guard of test-classes.py."""
    tests = dict(current['tests'])
    for proposal in facts['proposed']:
        tests[proposal['identity']] = proposal['entry']
    value = {'schema': current['schema'], 'tests': tests}
    tc.quarantine(cat, today, value)
    tc.partition(inventory, cat, tests)
    return value


def render(facts, wrote):
    lines = []
    verb = 'added' if wrote else 'would add'
    if facts['proposed']:
        lines.append(f'{verb} {len(facts["proposed"])} quarantine entries '
                     f'(since {facts["today"]}):')
        for proposal in facts['proposed']:
            entry = proposal['entry']
            lines.append(f'  {proposal["identity"]}: flaky in {proposal["tasks"]} Tasks, '
                         f'expires {entry["expires"]}, evidence {", ".join(entry["evidence"])}')
    else:
        lines.append('no identity was flaky in 2 distinct Tasks within the 7 days ending '
                      f'{facts["today"]}')
    if facts['over_cap']:
        lines.append(f'over the {tc.QUARANTINE_LIMIT}-entry cap, not added: '
                     + ', '.join(facts['over_cap']))
    for item in facts['rejected']:
        lines.append(f'rejected {item["name"]}: {item["reason"]}')
    if facts['proposed'] and not wrote:
        lines.append('nothing written; rerun with --write to add them')
    return '\n'.join(lines) + '\n'


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='command', required=True)
    run = sub.add_parser('scan', help='propose entries from af Tasks\' gate flaky lists')
    run.add_argument('--list', action='append', default=[], metavar='FILE',
                     help='an `af task list --json` document (repeatable)')
    run.add_argument('--state', action='append', default=[], metavar='DIR',
                     help='run `af task list --json --state DIR` (repeatable)')
    run.add_argument('--af', default='af', metavar='PATH', help='the af binary --state runs (default: af on PATH)')
    run.add_argument('--inventory', metavar='FILE',
                     help='the nextest list JSON (default: build the default-feature list)')
    run.add_argument('--today', metavar='YYYY-MM-DD',
                     help='today (default: AF_TEST_QUARANTINE_TODAY, else the clock)')
    run.add_argument('--quarantine', default=str(tc.QUARANTINE), metavar='FILE',
                     help='the quarantine file to read and write (default: .config/test-quarantine.json)')
    run.add_argument('--write', action='store_true', help='add the proposals to the quarantine file')
    run.add_argument('--json', action='store_true', help=f'print one {SCAN} document')
    args = parser.parse_args(argv)
    if not args.list and not args.state:
        parser.error('scan needs at least one --list FILE or --state DIR')
    today = parse_today(args.today)
    cat = tc.catalog()
    path = Path(args.quarantine)
    current = tc.read(path)
    tc.quarantine(cat, today, current)
    rows = read_lists(args.list, args.state, args.af)
    inventory = tc.read(args.inventory) if args.inventory else default_inventory()
    facts = scan(rows, inventory, current, today, cat)
    wrote = False
    if args.write and facts['proposed']:
        value = written(current, facts, inventory, today, cat)
        tc.write(path, value)
        wrote = True
    if args.json:
        print(json.dumps({**facts, 'written': wrote}, indent=2, sort_keys=True))
    else:
        sys.stdout.write(render(facts, wrote))
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        print(f'test-quarantine: {error}', file=sys.stderr)
        sys.exit(1)
