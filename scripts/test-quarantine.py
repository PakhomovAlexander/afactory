#!/usr/bin/env python3
"""Propose, confirm and settle test quarantine entries (ADR-0268; stdlib only).

    scripts/test-quarantine.py scan --list FILE | --state DIR [...] [--inventory FILE]
                                    [--today YYYY-MM-DD] [--write] [--json]
    scripts/test-quarantine.py confirm --report FILE [--runs N] [--only IDENTITY ...]
                                       [--test-threads N] [--load functional|none]
                                       [--cargo PATH] [--inventory FILE] [--today YYYY-MM-DD]
    scripts/test-quarantine.py apply REPORT [--write] [--issues DIR]
                                     [--evidence-platform macos|linux] [--inventory FILE]
                                     [--today YYYY-MM-DD]

`scan` reads `af task list --json` documents (`af/task-list@3` or `@4`), resolves each row's
`gate.flaky` names against the default-feature nextest inventory and proposes, as `quarantined`,
every identity flaky in at least 2 distinct Tasks updated within the 7 days ending at the end of
today (UTC). Without --write it changes nothing; --write adds the proposals to
.config/test-quarantine.json through scripts/test-classes.py's guards, or writes nothing.

`confirm` runs the quarantined tests (or the --only ones) in N whole nextest runs, never
stopping early and never rerunning a failure, with the functional class's selection running in
a loop beside them unless --load none, and writes one af.test-quarantine-confirm/1 report:
`flaky` (failed in some runs), `always_fails` (failed in every run) or `clean` (never failed).
A run without a JUnit, or whose JUnit lacks an identity, writes no report.

`apply` applies such a report to the quarantine file: `flaky` becomes `state: flaky` for 14
days, `always_fails` and `clean` are removed, except that a clean report from a host other than
the evidence platform (--evidence-platform, default macos) keeps the entry. --write writes the
file and, into --issues DIR, one issue body per flaky and always-failing entry; without it
nothing changes. It never runs gh.
"""
import argparse
import datetime as dt
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import xml.etree.ElementTree as ET

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location('test_classes', ROOT / 'scripts/test-classes.py')
tc = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tc)
GATE_SPEC = importlib.util.spec_from_file_location('nextest_gate', ROOT / 'scripts/nextest-gate.py')
ng = importlib.util.module_from_spec(GATE_SPEC)
GATE_SPEC.loader.exec_module(ng)

LISTS = ('af/task-list@3', 'af/task-list@4')
SCAN = 'af.test-quarantine-scan/1'
MIN_TASKS = 2
WINDOW_DAYS = 7
MAX_EVIDENCE = 10
NOT_A_TEST = 'not a test'
CONFIRM = 'af.test-quarantine-confirm/1'
CLASSIFICATIONS = ('flaky', 'always_fails', 'clean')
OUTPUT_LIMIT = 64 * 1024
# The platform.system() of the gates an entry's evidence comes from: rows carry no platform, and
# this repository's af gates run locally on macOS (AGENTS.md).
EVIDENCE_PLATFORMS = {'macos': 'Darwin', 'linux': 'Linux'}


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


def default_inventory(cargo='cargo'):
    """The default-feature nextest list, built as test-classes.py's build_inventory builds it."""
    result = subprocess.run([cargo, 'nextest', 'list', '--workspace', '--locked',
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


def exact_filter(identities):
    """The nextest filter selecting exactly `identities`, as the isolation rerun names one test."""
    return ' | '.join(f'(binary_id(={binary}) & test(={name}))'
                      for binary, name in sorted(identity.split(' ') for identity in identities))


def bounded(text):
    """(text, truncated): `text` cut to OUTPUT_LIMIT bytes of UTF-8."""
    data = text.encode()
    if len(data) <= OUTPUT_LIMIT:
        return text, False
    return data[:OUTPUT_LIMIT].decode(errors='ignore'), True


def junit_results(junit):
    """identity -> None (passed) or its failure output, for every case the JUnit ran; None when
    there is no readable JUnit."""
    # nextest-gate.py decides what failed, as it does for the gate; this adds what ran and the output.
    failed = ng.failed_tests(junit)
    if failed is None:
        return None
    failed = {f'{binary} {name}' for binary, name in failed}
    root = ET.parse(junit).getroot()
    results = {}
    for suite in root.iter('testsuite'):
        for case in suite.iter('testcase'):
            if case.find('skipped') is not None:
                continue  # Not run: no result.
            identity = f'{case.get("classname") or suite.get("name") or ""} {case.get("name") or ""}'
            if identity not in failed:
                results[identity] = None
                continue
            failures = [*case.findall('failure'), *case.findall('error')]
            parts = [(failure.text or '').strip() or failure.get('message') or failure.tag
                     for failure in failures]
            for stream in ('system-out', 'system-err'):
                for element in case.findall(stream):
                    if (element.text or '').strip():
                        parts.append(f'--- {stream} ---\n{element.text.strip()}')
            results[identity] = '\n'.join(parts)
    return results


def git_commit():
    result = subprocess.run(['git', 'rev-parse', 'HEAD'], capture_output=True, text=True, cwd=ROOT)
    commit = result.stdout.strip()
    if result.returncode != 0 or not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', commit):
        raise ValueError('confirm: cannot read the commit to confirm (git rev-parse HEAD: '
                         f'{result.stderr.strip() or commit or "exit " + str(result.returncode)}); '
                         'fix: run it in a git checkout of that commit')
    return commit


class Load:
    """The functional class's selection, run in a loop beside the confirmation runs until they
    end. Its results are never read: its own failures do not count."""

    def __init__(self, command, directory):
        self.command, self.directory = command, directory
        self.lock, self.stopping, self.started = threading.Lock(), False, threading.Event()
        self.process, self.runs = None, 0
        self.thread = threading.Thread(target=self.loop, daemon=True)

    def loop(self):
        config = self.directory / 'store.toml'
        config.write_text('[store]' + chr(10) + 'dir = ' + json.dumps(str(self.directory / 'store'))
                          + chr(10))
        try:
            with (self.directory / 'load.log').open('ab') as log:
                while True:
                    with self.lock:
                        if self.stopping:
                            return
                        self.process = subprocess.Popen(
                            self.command + ['--tool-config-file', f'af-gate:{config}'],
                            stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
                        self.runs += 1
                    self.started.set()
                    self.process.wait()
        finally:
            self.started.set()

    def start(self):
        """Start the loop and return once its first run is alive, so every confirmation run has
        the load beside it."""
        self.thread.start()
        self.started.wait()

    def stop(self):
        with self.lock:
            self.stopping = True
            process = self.process
        if process is not None and process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        self.thread.join()


def confirm(identities, runs, threads, cargo, load_expression, work):
    """identity -> its failed runs and first failure over `runs` whole nextest runs of exactly
    `identities`, and the load's run count. Every run runs: none stops the others and no failure
    reruns. Raises, naming the run, when a run has no JUnit or its JUnit lacks an identity."""
    base = [cargo, 'nextest', 'run', '--workspace', '--locked', '--profile', 'ci', '--no-fail-fast',
            '--retries', '0', '--test-threads', str(threads)]
    load = None
    if load_expression is not None:
        (work / 'load').mkdir(parents=True)
        load = Load([*base, '-E', load_expression], work / 'load')
        load.start()
    results = {identity: {'failed_runs': [], 'first_failure': None} for identity in identities}
    try:
        for index in range(1, runs + 1):
            print(f'confirm: run {index} of {runs}', flush=True)
            code, _, junit = ng.run_nextest([*base, '-E', exact_filter(identities)], work,
                                            work / f'run-{index}', Path('ci') / 'junit.xml', False)
            cases = junit_results(junit)
            if cases is None:
                raise ValueError(f'confirm: run {index} of {runs} ended without a JUnit (nextest exit '
                                 f'{code}); no report written: a missing result is not a pass')
            missing = sorted(set(identities) - set(cases))
            if missing:
                raise ValueError(f'confirm: run {index} of {runs}: its JUnit has no result for '
                                 f'{", ".join(missing)}; no report written: a missing result is not '
                                 'a pass')
            failed = [identity for identity in identities if cases[identity] is not None]
            if code != 0 and not failed:
                raise ValueError(f'confirm: run {index} of {runs}: nextest exit {code} with every '
                                 'test passing; no report written')
            for identity in failed:
                result = results[identity]
                result['failed_runs'].append(index)
                if result['first_failure'] is None:
                    output, truncated = bounded(cases[identity])
                    result['first_failure'] = {'run': index, 'output': output, 'truncated': truncated}
            if failed:
                print(f'confirm: run {index} of {runs} failed: {", ".join(failed)}', flush=True)
    finally:
        if load is not None:
            load.stop()
    for result in results.values():
        count = len(result['failed_runs'])
        result['failures'] = count
        result['classification'] = ('clean' if count == 0 else
                                    'always_fails' if count == runs else 'flaky')
    return results, (load.runs if load else 0)


def run_confirm(args):
    if args.runs < 1:
        raise ValueError(f'confirm: --runs {args.runs} is not a positive number of runs')
    if args.test_threads < 1:
        raise ValueError(f'confirm: --test-threads {args.test_threads} is not a positive thread count')
    report = Path(args.report)
    if report.exists():
        raise ValueError(f'confirm: {report} exists; fix: name a new report file')
    today = parse_today(args.today)
    cat = tc.catalog()
    held = tc.quarantine(cat, today, tc.read(args.quarantine))
    absent = sorted(set(args.only) - set(held))
    if absent:
        raise ValueError(f'confirm: --only {absent[0]} is not in {args.quarantine}')
    identities = sorted(set(args.only) or held)
    if not identities:
        raise ValueError(f'confirm: {args.quarantine} lists no test to confirm')
    commit = git_commit()
    expression = None
    if args.load == 'functional':
        inventory = tc.read(args.inventory) if args.inventory else default_inventory(args.cargo)
        # The gate's own selection: the functional class of the audited inventory, which no held
        # identity is in, because each classifies as quarantine.
        expression = tc.filterset(tc.partition(inventory, cat, held), ['functional'])
    work = Path(tempfile.mkdtemp(prefix='af-quarantine-confirm-'))
    try:
        results, load_runs = confirm(identities, args.runs, args.test_threads, args.cargo,
                                     expression, work)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    document = {'schema': CONFIRM, 'commit': commit, 'today': today.isoformat(),
                'host': {'system': platform.system(), 'machine': platform.machine(),
                         'cpus': os.cpu_count() or 1, 'test_threads': args.test_threads},
                'load': {'selection': args.load, 'runs': load_runs}, 'runs': args.runs,
                'tests': {identity: {key: results[identity][key] for key in
                                     ('failures', 'failed_runs', 'classification', 'first_failure')}
                          for identity in identities}}
    with report.open('x') as stream:
        stream.write(json.dumps(document, indent=2, sort_keys=True) + '\n')
    for identity in identities:
        result = results[identity]
        print(f'{result["classification"]}: {identity} (failed in {result["failures"]} of '
              f'{args.runs} runs)')
    print(f'confirm: wrote {report}')
    return 0


def read_report(path):
    """(report, its today): a confirm report, refused unless it is exactly
    af.test-quarantine-confirm/1 and its classifications follow from its failed runs."""
    try:
        value = tc.read(path)
    except json.JSONDecodeError as error:
        raise ValueError(f'{path}: not JSON: {error}') from None

    def refuse(problem):
        raise ValueError(f'{path}: {problem}; fix: apply a report scripts/test-quarantine.py '
                         'confirm wrote')

    def exact(item, keys):
        return isinstance(item, dict) and set(item) == set(keys)

    def count(item, low=0):
        return type(item) is int and item >= low

    if not exact(value, ('schema', 'commit', 'today', 'host', 'load', 'runs', 'tests')) \
            or value['schema'] != CONFIRM:
        schema = value.get('schema') if isinstance(value, dict) else None
        refuse(f'not an {CONFIRM} report (schema {schema!r})')
    if not isinstance(value['today'], str):
        refuse(f'`today` {value["today"]!r} is not a YYYY-MM-DD date')
    try:
        today = parse_today(value['today'])
    except ValueError:
        refuse(f'`today` {value["today"]!r} is not a YYYY-MM-DD date')
    host, load, runs = value['host'], value['load'], value['runs']
    if not (exact(host, ('system', 'machine', 'cpus', 'test_threads'))
            and isinstance(host['system'], str) and isinstance(host['machine'], str)
            and count(host['cpus'], 1) and count(host['test_threads'], 1)):
        refuse('`host` is not {system, machine, cpus, test_threads}')
    if not (exact(load, ('selection', 'runs')) and load['selection'] in ('functional', 'none')
            and count(load['runs'])):
        refuse('`load` is not {selection, runs}')
    if (not isinstance(value['commit'], str) or not count(runs, 1)
            or not isinstance(value['tests'], dict) or not value['tests']):
        refuse('`commit`, `runs` or `tests` is malformed')
    for identity, result in value['tests'].items():
        if not exact(result, ('failures', 'failed_runs', 'classification', 'first_failure')):
            refuse(f'{identity}: not {{failures, failed_runs, classification, first_failure}}')
        failed = result['failed_runs']
        if (not isinstance(failed, list) or not all(count(i, 1) and i <= runs for i in failed)
                or failed != sorted(set(failed)) or result['failures'] != len(failed)):
            refuse(f'{identity}: `failures` and `failed_runs` disagree')
        expected = 'clean' if not failed else 'always_fails' if len(failed) == runs else 'flaky'
        if result['classification'] != expected:
            refuse(f'{identity}: classification {result["classification"]!r} for {len(failed)} of '
                   f'{runs} failed runs, expected {expected!r}')
        first = result['first_failure']
        if failed and not (exact(first, ('run', 'output', 'truncated')) and first['run'] == failed[0]
                           and isinstance(first['output'], str)
                           and type(first['truncated']) is bool):
            refuse(f'{identity}: `first_failure` is not the output of run {failed[0]}')
        if not failed and first is not None:
            refuse(f'{identity}: a clean result with a `first_failure`')
    return value, today


def slug(identity):
    return re.sub(r'[^A-Za-z0-9_.-]+', '-', identity).strip('-')


def fenced(text):
    """`text` in a fence longer than any backtick run inside it."""
    longest = max((len(run) for run in re.findall(r'`+', text)), default=0)
    fence = '`' * max(3, longest + 1)
    return f'{fence}text\n{text}\n{fence}'


def issue_body(identity, result, report, entry, expires=None):
    """The Markdown body of the issue a confirmed entry files (`gh issue create --body-file`)."""
    runs, failed, host, load = report['runs'], result['failed_runs'], report['host'], report['load']
    first = result['first_failure']
    if result['classification'] == 'flaky':
        opening = (f'`{identity}` failed in {len(failed)} of {runs} runs of '
                   '`scripts/test-quarantine.py confirm` and passed in the others: it is flaky '
                   '(ADR-0268). Its quarantine entry is now `flaky` until '
                   f'{expires}, so no gate runs it.')
        fix = ('Fix its cause in its own change, prove the fix with `make test-repeat` '
               '(`CONTRIBUTING.md`), and remove the entry from `.config/test-quarantine.json`.')
    else:
        opening = (f'P0: main is broken. `{identity}` failed in every one of {runs} runs of '
                   '`scripts/test-quarantine.py confirm`: it is not flaky, it fails. Its quarantine '
                   'entry is removed, so it fails every gate again until it is fixed.')
        fix = 'Fix the test or the code it tests first: every gate fails until then.'
    output = first['output'] + ('\n[output truncated at 64 KiB]' if first['truncated'] else '')
    lines = [opening, '',
             f'- Failing runs: {len(failed)} of {runs} (runs {", ".join(map(str, failed))}).',
             f'- Commit: `{report["commit"]}`, confirmed on {report["today"]}.',
             f'- Host: {host["system"]} {host["machine"]}, {host["cpus"]} CPUs, '
             f'{host["test_threads"]} test threads; load: {load["selection"]} '
             f'({load["runs"]} runs beside the confirmation).',
             '- Evidence Tasks (their gates listed it flaky): '
             + ', '.join(f'`{task}`' for task in entry['evidence']) + '.',
             '', f'First failure, run {first["run"]}:', '', fenced(output), '', fix, '']
    return '\n'.join(lines)


def applied(report, current, today, evidence_platform):
    """(tests, actions): the quarantine file's tests after the report, and one action per
    reported identity."""
    tests = dict(current['tests'])
    absent = sorted(set(report['tests']) - set(tests))
    if absent:
        raise ValueError(f'apply: {absent[0]} is in the report but not in the quarantine file; '
                         'fix: confirm the current entries again')
    evidence = EVIDENCE_PLATFORMS[evidence_platform]
    system = report['host']['system']
    expires = (today + dt.timedelta(days=tc.QUARANTINE_DAYS)).isoformat()
    actions = []
    for identity in sorted(report['tests']):
        result, entry = report['tests'][identity], tests[identity]
        kind = result['classification']
        action = {'identity': identity, 'classification': kind, 'result': result, 'entry': entry}
        if kind == 'flaky':
            tests[identity] = {**entry, 'state': 'flaky', 'since': today.isoformat(),
                               'expires': expires}
            action.update(change='flaky', new=tests[identity], issue=f'flaky-{slug(identity)}.md',
                          body=issue_body(identity, result, report, entry, expires),
                          title=f'flaky: {identity}', label='flaky')
        elif kind == 'always_fails':
            del tests[identity]
            action.update(change='remove', issue=f'p0-{slug(identity)}.md',
                          body=issue_body(identity, result, report, entry),
                          title=f'P0: {identity} fails in every run', label='P0')
        elif system == evidence:
            del tests[identity]
            action.update(change='remove')
        else:
            action.update(change='keep', reason=(
                f'the report is from {system} and its evidence from {evidence} gates '
                f'(--evidence-platform {evidence_platform}); only a clean {evidence} confirmation '
                'releases it'))
        actions.append(action)
    return tests, actions


def quote(text):
    return "'" + text.replace("'", "'\\''") + "'"


def render_apply(actions, runs, issues, wrote):
    lines = []
    for action in actions:
        head = (f'{action["classification"]}: {action["identity"]} (failed in '
                f'{action["result"]["failures"]} of {runs} runs)')
        if action['change'] == 'flaky':
            new = action['new']
            what = (f'{"set" if wrote else "would set"} state flaky, since {new["since"]}, '
                    f'expires {new["expires"]}')
        elif action['change'] == 'remove':
            what = 'removed the entry' if wrote else 'would remove the entry'
        else:
            what = f'keeps the entry {action["entry"]["state"]}, its dates unchanged: {action["reason"]}'
        lines.append(f'{head}: {what}')
        if 'issue' in action:
            body = Path(issues) / action['issue'] if issues else action['issue']
            lines.append(f'  {"wrote" if wrote else "would write"} the {action["label"]} issue body {body}')
    issued = [action for action in actions if 'issue' in action]
    if wrote and issued:
        lines.append('file each issue:')
        lines.extend(f'  gh issue create --title {quote(action["title"])} --label {action["label"]} '
                     f'--body-file {quote(str(Path(issues) / action["issue"]))}' for action in issued)
    if not wrote and any(action['change'] != 'keep' for action in actions):
        lines.append('nothing written; rerun with --write --issues DIR to apply it')
    return '\n'.join(lines) + '\n'


def run_apply(args):
    report, reported = read_report(args.report)
    today = parse_today(args.today)
    if reported > today:
        raise ValueError(f'apply: the report is from {reported}, later than today ({today}); '
                         'fix: apply it on or after the day confirm wrote it')
    cat = tc.catalog()
    path = Path(args.quarantine)
    current = tc.read(path)
    tc.quarantine(cat, today, current)
    tests, actions = applied(report, current, today, args.evidence_platform)
    if args.write:
        issued = [action for action in actions if 'issue' in action]
        if issued and not args.issues:
            raise ValueError('apply: the report files issues; fix: name the directory for their '
                             'bodies with --issues DIR')
        targets = [Path(args.issues) / action['issue'] for action in issued]
        present = [target for target in targets if target.exists()]
        if present:
            raise ValueError(f'apply: {present[0]} exists; fix: name a new --issues directory')
        # Every guard runs before anything is written, partition's inventory guard included.
        value = {'schema': current['schema'], 'tests': tests}
        tc.quarantine(cat, today, value)
        inventory = tc.read(args.inventory) if args.inventory else default_inventory()
        tc.partition(inventory, cat, tests)
        for action, target in zip(issued, targets):
            target.parent.mkdir(parents=True, exist_ok=True)
            with target.open('x') as stream:
                stream.write(action['body'])
        if tests != current['tests']:
            tc.write(path, value)
    sys.stdout.write(render_apply(actions, report['runs'], args.issues, args.write))
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='command', required=True)
    today_help = 'today (default: AF_TEST_QUARANTINE_TODAY, else the clock)'
    inventory_help = 'the nextest list JSON (default: build the default-feature list)'
    file_help = 'the quarantine file to read and write (default: .config/test-quarantine.json)'
    run = sub.add_parser('scan', help='propose entries from af Tasks\' gate flaky lists')
    run.add_argument('--list', action='append', default=[], metavar='FILE',
                     help='an `af task list --json` document (repeatable)')
    run.add_argument('--state', action='append', default=[], metavar='DIR',
                     help='run `af task list --json --state DIR` (repeatable)')
    run.add_argument('--af', default='af', metavar='PATH', help='the af binary --state runs (default: af on PATH)')
    run.add_argument('--inventory', metavar='FILE', help=inventory_help)
    run.add_argument('--today', metavar='YYYY-MM-DD', help=today_help)
    run.add_argument('--quarantine', default=str(tc.QUARANTINE), metavar='FILE', help=file_help)
    run.add_argument('--write', action='store_true', help='add the proposals to the quarantine file')
    run.add_argument('--json', action='store_true', help=f'print one {SCAN} document')
    check = sub.add_parser('confirm', help='run the quarantined tests N times and classify each')
    check.add_argument('--report', required=True, metavar='FILE',
                       help=f'the {CONFIRM} report to write (an absent file)')
    check.add_argument('--runs', type=int, default=50, metavar='N', help='whole runs (default: 50)')
    check.add_argument('--only', action='append', default=[], metavar='IDENTITY',
                       help='confirm only this entry (repeatable; default: every entry)')
    check.add_argument('--test-threads', type=int, default=os.cpu_count() or 1, metavar='N',
                       help='nextest threads per run (default: the CPU count)')
    check.add_argument('--load', choices=('functional', 'none'), default='functional',
                       help='run the functional class in a loop beside the runs (default), or nothing')
    check.add_argument('--cargo', default='cargo', metavar='PATH',
                       help='the cargo every nextest run starts (default: cargo on PATH)')
    check.add_argument('--inventory', metavar='FILE', help=inventory_help + ', for the load')
    check.add_argument('--today', metavar='YYYY-MM-DD', help=today_help)
    check.add_argument('--quarantine', default=str(tc.QUARANTINE), metavar='FILE',
                       help='the quarantine file (default: .config/test-quarantine.json)')
    settle = sub.add_parser('apply', help='apply a confirm report to the quarantine file')
    settle.add_argument('report', metavar='REPORT', help=f'an {CONFIRM} report')
    settle.add_argument('--write', action='store_true',
                        help='write the quarantine file and the issue bodies')
    settle.add_argument('--issues', metavar='DIR', help='where --write puts the issue bodies')
    settle.add_argument('--evidence-platform', choices=tuple(EVIDENCE_PLATFORMS), default='macos',
                        help='the platform of the gates the entries\' evidence comes from (default: macos)')
    settle.add_argument('--inventory', metavar='FILE', help=inventory_help)
    settle.add_argument('--today', metavar='YYYY-MM-DD', help=today_help)
    settle.add_argument('--quarantine', default=str(tc.QUARANTINE), metavar='FILE', help=file_help)
    args = parser.parse_args(argv)
    if args.command == 'confirm':
        return run_confirm(args)
    if args.command == 'apply':
        return run_apply(args)
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
    if args.write:
        # Every guard runs, partition's inventory guard included, even with nothing to add.
        value = written(current, facts, inventory, today, cat)
        if facts['proposed']:
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
