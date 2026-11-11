#!/usr/bin/env python3
"""The Telemetry overhead gate: `off` against `spans` on the pinned bench fixtures (design §12).

    telemetry-overhead.py [--af PATH] [--pairs N] [--fixture NAME]... [--scale full|smoke]
                          [--seed N] [--work DIR] [--keep] [--json] [--report PATH]
    telemetry-overhead.py --judge DOCUMENT [--json]

Each measurement is a pair of commands on the same machine and fixture, one at `[telemetry]
level = "off"` and one at `"spans"`, in an order drawn at random per pair (`--seed`), 20 pairs
per fixture by default. The Store fixture is two sequential native Tasks (1,000 checks total);
both finalizations are timed and peak RSS is the maximum of their `wait4` high-water marks.
A command's wall time runs from exec to exit, so Telemetry's
finalization (the seal, the compression, the sweep's telemetry inventory) is inside it. Every
command runs in a fresh copy of its fixture's committed repository, with a fresh private `HOME`
and XDG directories, `TMPDIR` and Store below the work directory: nothing reads or changes the
operator's own af state, and no command finds what an earlier one left. A run must exit as its
fixture expects, leave no telemetry directory at `off` and a segment at `spans`; anything else
stops the measurement.

Per fixture the report gives the paired differences, `(spans - off) / off` in percent, their
median and a 95% percentile-bootstrap interval of the median (10,000 resamples, seeded). Drain
latency is the second figure of `telemetry-bench-dense`, named for its boundaries
(`harness_last_write_to_command_exit`): from the scripted harness's last write (the
`CLOCK_MONOTONIC` reading its final stderr line names, `last_write_monotonic_ns`) to this
script's own `CLOCK_MONOTONIC` reading right after `wait4` returned the command's exit, so the
two are causally ordered on one clock and no file timestamp is read; it covers the drain, the
rest of the command and its exit, never the Store's commit time alone. `spans` minus `off` per
pair in milliseconds, reported the same way. Each run's drain is a physical duration and the
pair's difference a signed one: a negative duration is kept as measured and leaves the gate
`not_judged`, never clamped or dropped. One explicit current fixture contract,
`native-task-review@1`, admits exact current package pins and requires the measured binary to
plan the workload before timing. The release string is provenance, never capability proof.
Unsupported formats are refused without rewriting receipts, packages or pins.

The gate passes when every fixture's median is at most 2% and its interval's upper bound at
most 3%, and the drain latency's median difference is at most 10 ms. It is judged only over all
three fixtures at `--scale full` with at least 20 pairs; anything less (the one-pair smoke `make
check` runs) is reported `not_judged`. `--judge` recomputes every figure and the verdict from a
recorded `af/telemetry-overhead@3` document's own pairs. Exit status: 0 for `pass` or
`not_judged`, 3 for `fail`, 1 when a measurement or a document cannot be read.

Prints a Markdown table, or with `--json` the `af/telemetry-overhead@3` document
(`schemas/telemetry-overhead-v3.json`); `--report PATH` writes the document as well.
"""
import argparse
import json
import os
from pathlib import Path
import platform
import random
import re
import shutil
import signal
import sqlite3
import stat
import statistics
import subprocess
import sys
import tempfile
import time

SCHEMA = 'af/telemetry-overhead@3'
REPOSITORY = Path(__file__).resolve().parent.parent
FIXTURES_DIR = REPOSITORY / 'fixtures' / 'telemetry-bench'
LEVELS = ('off', 'spans')
DEFAULT_PAIRS = 20
DEFAULT_SEED = 20261021
RESAMPLES = 10000
MEDIAN_BOUND_PCT = 2.0
UPPER_BOUND_PCT = 3.0
DRAIN_BOUND_MS = 10.0
DRAIN_FIXTURE = 'telemetry-bench-dense'
# The dense harness's answer on stdout, byte for byte (`fixtures/telemetry-bench/*/harness.py`).
REPLY = (b'{"schema":"af.worker-reply/1","outputs":{"result":[{"reports":[],'
         b'"benchmark_demands":[],"dispositions":[]}]}}\n')
LAST_WRITE = b'"subtype":"af_bench_last_write"'
# What a drain latency measures (`drain.boundary`): from the harness's last write, read on the
# host's `CLOCK_MONOTONIC` in the harness, to the measurement's own reading of that clock right
# after `wait4` returned the command's exit. Both name the clock explicitly: `time.monotonic_ns`
# is not one clock across Python builds (on macOS two builds read clocks days apart). The harness writes before it exits and the command
# exits only after it reaped the harness, so the two readings are causally ordered on one clock.
# It is not the Store's commit time: it includes the drain, the rest of the command and its exit.
DRAIN_BOUNDARY = 'harness_last_write_to_command_exit'
DRAIN_CLOCK = 'CLOCK_MONOTONIC'
# One benchmark contract, admitted from exact current schemas at source preflight and
# by the measured binary's own planner before timing. Release strings are provenance only:
# two source trees with the same release may accept different formats.
CURRENT_CONTRACT = 'native-task-review@1'
CHECK_RECEIPT_CURRENT = 'af/TaskCheckReceipt@2'
# Explicit release preparation guard; measurement capability still requires schema and planner admission.
CURRENT_RELEASES = ('0.15.1-rc.1', '0.15.1-rc.2', '0.15.1-rc.3')
CONTRACT_SCHEMAS = (
    'pipeline-definition-v6.json', 'compiled-task-v6.json', 'task-kind-v3.json',
    'task-check-receipt-v2.json', 'task-review-subject-v3.json',
)
# Each fixture: how its command is spelled and the exit it must end with. `{task}` is the
# fixture's Task file, `{state}` the run's own Store.
FIXTURES = {
    'telemetry-bench-concurrent': {
        'argv': ['task', 'start', '--execute', '--file', '{task}', '--state', '{state}', '--json'],
        'exit': 0,
    },
    'telemetry-bench-dense': {
        'argv': ['task', 'start', '--execute', '--file', '{task}', '--state', '{state}', '--json'],
        'exit': 0,
    },
    'telemetry-bench-store': {
        'argv': ['task', 'start', '--execute', '--file', '{task}', '--state', '{state}', '--json'],
        'exit': 0,
    },
}
# What `--scale smoke` changes in a fixture's `bench.json` harness: the same commands and the
# same protocol over far less output, so one pair of each fits `make check`. The Store fixture
# keeps ten named checks in one native node and its policy, re-pinning the copied package with
# the measured binary's canonical digest command.
SMOKE = {
    'telemetry-bench-concurrent': {'lines': 20},
    'telemetry-bench-dense': {'bytes': 1 << 20},
    'telemetry-bench-store': {'findings_per_reviewer': 10},
}
SMOKE_CHECKS = {'telemetry-bench-store': 10}
GIT_IDENTITY = {
    'GIT_AUTHOR_NAME': 'Telemetry Bench', 'GIT_AUTHOR_EMAIL': 'bench@example.invalid',
    'GIT_COMMITTER_NAME': 'Telemetry Bench', 'GIT_COMMITTER_EMAIL': 'bench@example.invalid',
    'GIT_AUTHOR_DATE': '2026-10-21T00:00:00Z', 'GIT_COMMITTER_DATE': '2026-10-21T00:00:00Z',
    'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_CONFIG_NOSYSTEM': '1',
}
# No background git work in a fixture repository: a commit otherwise starts a detached `git
# maintenance run --auto`, which holds `.git/objects/maintenance.lock` while a copy lists the
# repository and can run inside a measured command. Every `git` here carries these values, and
# `prepare` writes them into the pristine repository's config, which every copy inherits.
NO_BACKGROUND_GIT = (('gc.auto', '0'), ('maintenance.auto', 'false'),
                     ('gc.autoDetach', 'false'))
GIT_OPTIONS = [option for key, value in NO_BACKGROUND_GIT for option in ('-c', f'{key}={value}')]


class BenchError(Exception):
    """A measurement or a document that cannot be used; the message names it."""


# ------------------------------------------------------------------------------------------
# the rule


def bootstrap_interval(values, seed, label):
    """The 95% percentile-bootstrap interval of the median of `values`."""
    count = len(values)
    # Every resample of identical observations has this same median. Preserve the
    # ordinary even-count median arithmetic and rounding, including signed zero;
    # mixed representations take the original seeded 10,000-resample path below.
    if count and all(value == values[0] and repr(value) == repr(values[0])
                     for value in values):
        centre = round(statistics.median(values), 3)
        return [centre, centre]
    rng = random.Random(f'{seed}:{label}')
    medians = sorted(statistics.median([values[rng.randrange(count)] for _ in range(count)])
                     for _ in range(RESAMPLES))
    return [round(medians[int(RESAMPLES * 0.025)], 3),
            round(medians[int(RESAMPLES * 0.975) - 1], 3)]


def negative_drains(records, keys):
    """How many of the records' physical drain durations under `keys` are below zero."""
    return sum(1 for record in records for key in keys
               if record.get(key) is not None and record[key] < 0)


def summarize(document):
    """Recompute every difference, median, interval and the verdict from the recorded pairs."""
    protocol = document['protocol']
    seed = protocol['seed']
    # The bounds and the resampling are the protocol's, never a document's own.
    drain = document.get('drain')
    if ((protocol['median_bound_pct'], protocol['upper_bound_pct'], protocol['resamples'])
            != (MEDIAN_BOUND_PCT, UPPER_BOUND_PCT, RESAMPLES)
            or (drain is not None and drain['bound_ms'] != DRAIN_BOUND_MS)):
        raise BenchError('the document names other bounds or resamples than the protocol')
    if drain is not None and (drain['boundary'], drain['clock']) != (DRAIN_BOUNDARY, DRAIN_CLOCK):
        raise BenchError('the document measures its drain between other boundaries than the '
                         f'protocol ({DRAIN_BOUNDARY} on the {DRAIN_CLOCK} clock)')
    validate_contract(document)
    reasons, invalid_drains = [], []
    for fixture in document['fixtures']:
        differences = []
        for pair in fixture['pairs']:
            off, spans = pair['off']['wall_ms'], pair['spans']['wall_ms']
            if off <= 0:
                raise BenchError(f"{fixture['name']}: an `off` wall time of {off} ms")
            pair['difference_pct'] = round((spans - off) / off * 100, 3)
            differences.append(pair['difference_pct'])
        if not differences:
            raise BenchError(f"{fixture['name']}: no pairs")
        fixture['median_pct'] = round(statistics.median(differences), 3)
        fixture['interval_pct'] = bootstrap_interval(differences, seed, fixture['name'])
        fixture['passed'] = (fixture['median_pct'] <= protocol['median_bound_pct']
                             and fixture['interval_pct'][1] <= protocol['upper_bound_pct'])
        if not fixture['passed']:
            reasons.append(
                f"{fixture['name']}: median {fixture['median_pct']:+.3f}% (bound "
                f"{protocol['median_bound_pct']}%), interval upper bound "
                f"{fixture['interval_pct'][1]:+.3f}% (bound {protocol['upper_bound_pct']}%)")
    if drain is not None:
        differences = []
        for pair in drain['pairs']:
            pair['difference_ms'] = round(pair['spans_ms'] - pair['off_ms'], 3)
            differences.append(pair['difference_ms'])
        if not differences:
            raise BenchError('drain: no pairs')
        # A drain is a physical duration: a negative one is an ordering failure of the
        # measurement, kept in the pairs as measured and never clamped, filtered or judged.
        negative = negative_drains(drain['pairs'], ('off_ms', 'spans_ms'))
        drain['median_ms'] = round(statistics.median(differences), 3)
        drain['interval_ms'] = bootstrap_interval(differences, seed, 'drain')
        drain['passed'] = drain['median_ms'] <= drain['bound_ms']
        if negative:
            invalid_drains = [f'drain latency: {negative} negative physical duration'
                              f"{'s' if negative != 1 else ''}, kept as measured and not judged"]
        elif not drain['passed']:
            reasons.append(f"drain latency: median difference {drain['median_ms']:+.3f} ms "
                           f"(bound {drain['bound_ms']} ms)")
    names = sorted(fixture['name'] for fixture in document['fixtures'])
    unjudged = list(invalid_drains)
    if names != sorted(FIXTURES):
        unjudged.append(f'{len(names)} of the {len(FIXTURES)} fixtures measured')
    if drain is None:
        unjudged.append('no drain latency measured')
    if protocol['scale'] != 'full':
        unjudged.append(f"scale `{protocol['scale']}`, not `full`")
    fewest = min(len(fixture['pairs']) for fixture in document['fixtures'])
    if fewest < DEFAULT_PAIRS:
        unjudged.append(f'{fewest} pairs, fewer than {DEFAULT_PAIRS}')
    if unjudged:
        document['gate'] = {'verdict': 'not_judged', 'reasons': unjudged + reasons}
    elif reasons:
        document['gate'] = {'verdict': 'fail', 'reasons': reasons}
    else:
        document['gate'] = {'verdict': 'pass', 'reasons': []}
    return document


def judge(path):
    try:
        document = json.loads(Path(path).read_text(encoding='utf-8'))
    except (OSError, ValueError) as error:
        raise BenchError(f'cannot read {path}: {error}') from None
    if not isinstance(document, dict) or document.get('schema') != SCHEMA:
        raise BenchError(f'{path}: not an {SCHEMA} document')
    try:
        return summarize(document)
    except (KeyError, TypeError) as error:
        raise BenchError(f'{path}: not an {SCHEMA} document ({error!r})') from None


# ------------------------------------------------------------------------------------------
# the measurement


def private_env(run, level):
    """The command's environment: the caller's, minus every af, XDG and OTel setting, plus
    private homes below `run` and the level under test."""
    env = {name: value for name, value in os.environ.items()
           if not name.startswith(('AF_', 'XDG_', 'OTEL_', 'GIT_'))
           and name not in ('HOME', 'TMPDIR')}
    env.update({
        'HOME': str(run / 'home'), 'XDG_CONFIG_HOME': str(run / 'config'),
        'XDG_STATE_HOME': str(run / 'xstate'), 'XDG_DATA_HOME': str(run / 'data'),
        'XDG_CACHE_HOME': str(run / 'cache'), 'XDG_BIN_HOME': str(run / 'bin'),
        'TMPDIR': str(run / 'tmp'), 'AF_SELF_OFFLINE': '1', 'NO_COLOR': '1',
        'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_CONFIG_NOSYSTEM': '1',
        'AF_TELEMETRY__LEVEL': level,
    })
    return env


def keep_checks(af, repo, count):
    """Smoke: ten checks over two bounded Tasks, with an exact policy and canonical pin."""
    if count % 2:
        raise BenchError('the two-Task Store smoke requires an even total check count')
    count //= 2
    pipeline = repo / '.af/task-packages/bench/checks/pipeline.toml'
    head, *nodes = pipeline.read_text().split('[[nodes]]\n')
    first = nodes[0].split('\n[outputs.result]')[0]
    names = ', '.join(f'"check-{i:02d}"' for i in range(count))
    first = re.sub(r'checks = \[.*?\]', f'checks = [{names}]', first)
    pipeline.write_text(head + '[[nodes]]\n' + first
                        + '\n[outputs.result]\nkind = "node"\nnode = "check-00"\nport = "result"\n')
    policy = repo / '.af/code-policy.toml'
    prefix, *checks = re.split(r'(?=\n\[checks\.check-\d+\]\n)', policy.read_text())
    policy.write_text(prefix + ''.join(checks[:count]))
    catalog = repo / '.af/task-catalog.toml'
    text = catalog.read_text()
    digest = af_digest(af, repo, '.af/task-packages/bench/checks')
    text = re.sub(r'(\[packages\."bench/checks"\]\nversion = "[^"]+"\ndigest = ")[^"]+',
                  lambda match: match[1] + digest, text)
    catalog.write_text(text)
    return digest


def task_files(fixture):
    """The Store fixture retains 1,000 checks within unchanged native graph bounds."""
    first = FIXTURES_DIR / fixture / 'task.json'
    return [first, first.with_name('task-second.json')] if fixture == 'telemetry-bench-store' else [first]


def copy_writable(source, destination):
    """Copy `source` to `destination` with user write on every directory and file it creates.

    The gate materializes the source tree read-only and `copytree` keeps those modes, so a copy
    that is written to (the smoke's `bench.json`, the run's commit) or removed afterwards must
    get its write permission back. Symbolic links are copied as links and never followed.
    """
    shutil.copytree(source, destination, symlinks=True)
    for directory, _, files in os.walk(destination):
        os.chmod(directory, os.stat(directory).st_mode | stat.S_IRWXU)
        for name in files:
            path = os.path.join(directory, name)
            if not os.path.islink(path):
                os.chmod(path, os.stat(path).st_mode | stat.S_IRUSR | stat.S_IWUSR)


def release_version(version_line):
    """The release `af --version` names: `af 0.13.0` is `0.13.0`."""
    words = version_line.split()
    if len(words) < 2 or words[0] != 'af':
        raise BenchError(f'`af --version` printed {version_line!r}, not `af VERSION`')
    return words[1]


def select_contract(requested=None):
    """Require the one current workload; a release number never establishes capability."""
    if requested is not None and requested != CURRENT_CONTRACT:
        raise BenchError(f'unsupported fixture contract `{requested}`; require `{CURRENT_CONTRACT}`')
    return CURRENT_CONTRACT


def validate_contract(document):
    contract = document['fixture_contract']
    select_contract(contract['name'])
    if (contract['check_receipt'] != CHECK_RECEIPT_CURRENT
            or release_version(document['af']['version']) != contract['release']):
        raise BenchError('fixture contract receipt or release provenance does not match the document')


def af_digest(af, repo, path):
    run = repo.parent / 'contract'
    for name in ('home', 'config', 'xstate', 'data', 'cache', 'bin', 'tmp'):
        (run / name).mkdir(parents=True, exist_ok=True)
    try:
        done = subprocess.run([af, 'catalog', 'digest', path], cwd=repo, capture_output=True,
                              text=True, env=private_env(run, 'off'))
    finally:
        shutil.rmtree(run, ignore_errors=True)
    if done.returncode != 0:
        raise BenchError(f'{af} catalog digest {path}: {done.stderr.strip()}')
    return done.stdout.strip()


def apply_contract(af, fixture, repo, name):
    """Validate every committed package pin with the measured binary; rewrite nothing."""
    select_contract(name)
    catalog = repo / '.af/task-catalog.toml'
    text = catalog.read_text(encoding='utf-8')
    entries = re.findall(r'^\[packages\."([^"]+)"\]\nversion = "[^"]*"\ndigest = "([^"]+)"\n'
                         r'path = "([^"]+)"$', text, re.M)
    if not entries or len(entries) != text.count('[packages.'):
        raise BenchError(f'{catalog}: not a catalog of pinned packages')
    packages = {}
    for package, expected, path in entries:
        digest = af_digest(af, repo, path)
        if digest != expected:
            raise BenchError(f'{fixture}: {package} under `{name}` has digest {digest}, '
                             f'not the declared {expected}')
        packages[package] = digest
    return {'fixture': fixture, 'packages': packages}


def admit_fixture(af, fixture, repo, environment=None, task=None):
    """Compile the exact workload with the measured binary, outside the timed command.

    No Worker runs. Current Pipeline, kinds, signatures and receipt ports must be accepted
    together. Unsupported binaries retain their own diagnostic; no alternate fixture is tried.
    """
    task = task or FIXTURES_DIR / fixture / 'task.json'
    expected_task = json.loads(task.read_bytes())['task_id']
    run = repo.parent / 'admission'
    for name in ('home', 'config', 'xstate', 'data', 'cache', 'bin', 'tmp'):
        (run / name).mkdir(parents=True, exist_ok=True)
    try:
        env = environment(run) if environment else private_env(run, 'off')
        done = subprocess.run([af, 'task', 'start', '--file', str(task),
                               '--state', str(run / 'store'), '--json'], cwd=repo,
                              capture_output=True, env=env)
        if done.returncode != 0:
            raise BenchError(f'{fixture}: current workload admission exit {done.returncode}:\n'
                             + done.stderr.decode('utf-8', errors='replace')[-4000:]
                             + done.stdout.decode('utf-8', errors='replace')[-4000:])
        try:
            document = json.loads(done.stdout)
            if (document['schema'] != 'af/task-inspection@20'
                    or type(document['attempts']) is not int or document['attempts'] != 0
                    or document['phase'] != {'kind': 'ready'}
                    or document['task_id'] != expected_task
                    or not isinstance(document['plan_id'], str)
                    or not re.fullmatch(r'sha256:[0-9a-f]{64}', document['plan_id'])):
                raise ValueError('not the exact ready zero-Attempt current Task plan')
        except (ValueError, KeyError, TypeError) as error:
            raise BenchError(f'{fixture}: malformed current workload admission: {error}') from error
    finally:
        shutil.rmtree(run, ignore_errors=True)


def prepare(fixture, scale, work, af, contract):
    """The fixture's repository under the fixture contract `contract`, committed once; every
    run copies it. Returns it with the contract's provenance (`apply_contract`)."""
    source = FIXTURES_DIR / fixture
    pristine = work / fixture / 'pristine'
    copy_writable(source / 'repo', pristine)
    produced = apply_contract(af, fixture, pristine, contract)
    if scale == 'smoke':
        bench = pristine / 'bench.json'
        parameters = json.loads(bench.read_text(encoding='utf-8'))
        parameters['harness'].update(SMOKE[fixture])
        bench.write_text(json.dumps(parameters, indent=2) + '\n', encoding='utf-8')
        if fixture in SMOKE_CHECKS:
            produced['packages']['bench/checks'] = keep_checks(af, pristine, SMOKE_CHECKS[fixture])
    env = {name: value for name, value in os.environ.items() if not name.startswith('GIT_')}
    env.update(GIT_IDENTITY)
    for args in (['init', '-q', '-b', 'main'],
                 *(['config', key, value] for key, value in NO_BACKGROUND_GIT),
                 ['add', '-A'], ['commit', '-qm', fixture]):
        done = subprocess.run(['git', *GIT_OPTIONS, *args], cwd=pristine, env=env,
                              capture_output=True, text=True)
        if done.returncode != 0:
            raise BenchError(f'{fixture}: git {" ".join(args)}: {done.stderr.strip()}')
    locks = sorted(str(path) for path in (pristine / '.git').rglob('*.lock'))
    if locks:
        raise BenchError(f'{fixture}: a git lock remains after the commit: {", ".join(locks)}')
    for task in task_files(fixture):
        admit_fixture(af, fixture, pristine, task=task)
    return pristine, produced


def store_events(state):
    """Every event the run's Store databases hold, or None when none can be read."""
    total, found = 0, False
    for database in sorted(state.rglob('events.sqlite')):
        try:
            connection = sqlite3.connect(f'file:{database}?mode=ro', uri=True)
            try:
                total += connection.execute('SELECT count(*) FROM events').fetchone()[0]
                found = True
            finally:
                connection.close()
        except sqlite3.Error:
            continue
    return total if found else None


def last_write(state):
    """The dense harness's last write on `CLOCK_MONOTONIC`, from the stderr the Store holds.

    The Store must also hold the harness's drained answer: a run whose answer was not stored
    has no drain to measure."""
    objects = state / 'cas' / 'objects'
    answered, written = False, None
    for path in sorted(objects.glob('*/*')):
        if path.name.startswith('.') or not path.is_file():
            continue
        size = path.stat().st_size
        if size == len(REPLY) and path.read_bytes() == REPLY:
            answered = True
        elif size > len(LAST_WRITE):
            with path.open('rb') as file:
                file.seek(max(0, size - 4096))
                tail = file.read().rstrip(b'\n').rsplit(b'\n', 1)[-1]
            if LAST_WRITE in tail:
                written = json.loads(tail).get('last_write_monotonic_ns')
    if not answered or written is None:
        raise BenchError(f'{state}: the dense harness left no drained answer and last write '
                         'on CLOCK_MONOTONIC')
    return int(written)


def drain_latency(last_write_ns, exit_observed_ns):
    """Milliseconds from the harness's last write to the observed exit of its command
    (`DRAIN_BOUNDARY`), both readings of the one `CLOCK_MONOTONIC`. A negative duration is kept as
    measured: `summarize` leaves the drain unjudged rather than clamp it."""
    return round((exit_observed_ns - last_write_ns) / 1e6, 3)


def segments(run):
    """The telemetry segments the run left, sealed or not; None without a telemetry directory."""
    root = run / 'xstate' / 'af' / 'telemetry'
    if not root.exists():
        return None
    return sum(1 for path in (root / 'v1' / 'segments').glob('*')
               if path.name.endswith(('.jsonl.gz', '.jsonl')))


def run_once(af, fixture, pristine, work, label, level):
    run = work / fixture / label
    repo = run / 'repo'
    copy_writable(pristine, repo)
    for name in ('home', 'config', 'xstate', 'data', 'cache', 'bin', 'tmp'):
        (run / name).mkdir(parents=True)
    state = run / 'store'
    # The Store workload is two sequential native Tasks in one private Store: all 1,000
    # checks, with each graph below the existing 64-node bound. Both finalizations are timed.
    commands = [[af] + [part.format(task=task, state=state)
                        for part in FIXTURES[fixture]['argv']] for task in task_files(fixture)]
    peak_rss = 0
    with open(run / 'stdout', 'wb') as stdout, open(run / 'stderr', 'wb') as stderr:
        start = time.perf_counter_ns()
        for argv in commands:
            process = subprocess.Popen(argv, cwd=repo, env=private_env(run, level),
                                       stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr)
            _, status, usage = os.wait4(process.pid, 0)
            process.returncode = os.waitstatus_to_exitcode(status)
            peak_rss = max(peak_rss, usage.ru_maxrss)
            if process.returncode != FIXTURES[fixture]['exit']:
                break
        exit_observed_ns = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
        wall_ns = time.perf_counter_ns() - start
    if process.returncode != FIXTURES[fixture]['exit']:
        tail = (run / 'stderr').read_text(encoding='utf-8', errors='replace')[-2000:]
        raise BenchError(f'{fixture} {label}: exit {process.returncode}, expected '
                         f"{FIXTURES[fixture]['exit']}\n{tail}")
    sealed = segments(run)
    if level == 'off' and sealed is not None:
        raise BenchError(f'{fixture} {label}: a telemetry directory at `off`')
    if level != 'off' and not sealed:
        raise BenchError(f'{fixture} {label}: no telemetry segment at `{level}`')
    scale = 1 if sys.platform == 'darwin' else 1024
    result = {'wall_ms': round(wall_ns / 1e6, 3), 'exit_code': process.returncode,
              'peak_rss_bytes': peak_rss * scale, 'segments': sealed or 0}
    extra = {'store_events': store_events(state)}
    if fixture == DRAIN_FIXTURE:
        extra['drain_ms'] = drain_latency(last_write(state), exit_observed_ns)
    return result, extra


def measure(args):
    af = str(Path(args.af).resolve())
    fixtures = args.fixture or sorted(FIXTURES)
    work = Path(args.work).resolve() if args.work else Path(
        tempfile.mkdtemp(prefix='af-telemetry-overhead-'))
    work.mkdir(parents=True, exist_ok=True)
    version = subprocess.run([af, '--version'], capture_output=True, text=True,
                             env=private_env(work / 'version', 'off'))
    if version.returncode != 0:
        raise BenchError(f'{af} --version: {version.stderr.strip()}')
    release = release_version(version.stdout.strip())
    contract = select_contract(args.fixture_contract)
    order_rng = random.Random(args.seed)
    load = os.getloadavg()[0] if hasattr(os, 'getloadavg') else None
    document = {
        'schema': SCHEMA,
        'af': {'path': af, 'version': version.stdout.strip()},
        'fixture_contract': {'name': contract, 'release': release,
                             'check_receipt': CHECK_RECEIPT_CURRENT,
                             'fixtures': []},
        'machine': {'os': platform.system().lower(), 'arch': platform.machine(),
                    'cpus': os.cpu_count() or 0,
                    'load1': None if load is None else round(load, 2)},
        'protocol': {'pairs': args.pairs, 'scale': args.scale, 'seed': args.seed,
                     'resamples': RESAMPLES, 'median_bound_pct': MEDIAN_BOUND_PCT,
                     'upper_bound_pct': UPPER_BOUND_PCT},
        'fixtures': [],
        'drain': None,
    }
    try:
        for fixture in fixtures:
            pristine, produced = prepare(fixture, args.scale, work, af, contract)
            document['fixture_contract']['fixtures'].append(produced)
            entry = {'name': fixture, 'command': ' '.join(FIXTURES[fixture]['argv'][:3]),
                     'store_events': None, 'pairs': []}
            drains = []
            for index in range(args.pairs):
                order = list(LEVELS)
                order_rng.shuffle(order)
                pair = {'order': order}
                drain = {}
                for level in order:
                    label = f'{index:02d}-{level}'
                    pair[level], extra = run_once(af, fixture, pristine, work, label, level)
                    entry['store_events'] = extra['store_events']
                    if 'drain_ms' in extra:
                        drain[f'{level}_ms'] = extra['drain_ms']
                    if not args.keep:
                        shutil.rmtree(work / fixture / label, ignore_errors=True)
                entry['pairs'].append(pair)
                if drain:
                    drains.append(drain)
                print(f'{fixture}: pair {index + 1}/{args.pairs} off '
                      f"{pair['off']['wall_ms']:.0f} ms, spans {pair['spans']['wall_ms']:.0f} ms",
                      file=sys.stderr)
            document['fixtures'].append(entry)
            if drains:
                document['drain'] = {'fixture': DRAIN_FIXTURE, 'bound_ms': DRAIN_BOUND_MS,
                                     'boundary': DRAIN_BOUNDARY, 'clock': DRAIN_CLOCK,
                                     'pairs': drains}
    finally:
        if not args.keep and not args.work:
            shutil.rmtree(work, ignore_errors=True)
    return summarize(document)


# ------------------------------------------------------------------------------------------
# output


def table(document):
    lines = ['| Fixture | Pairs | Store events | Median `off` | Median `spans` | Median overhead '
             '| 95% interval | Verdict |',
             '| --- | ---: | ---: | ---: | ---: | ---: | --- | --- |']
    for fixture in document['fixtures']:
        offs = [pair['off']['wall_ms'] for pair in fixture['pairs']]
        spans = [pair['spans']['wall_ms'] for pair in fixture['pairs']]
        low, high = fixture['interval_pct']
        events = '' if fixture.get('store_events') is None else str(fixture['store_events'])
        lines.append(
            f"| `{fixture['name']}` | {len(fixture['pairs'])} | {events} | "
            f'{statistics.median(offs):.0f} ms | {statistics.median(spans):.0f} ms | '
            f"{fixture['median_pct']:+.2f}% | [{low:+.2f}%, {high:+.2f}%] | "
            f"{'pass' if fixture['passed'] else 'fail'} |")
    drain = document.get('drain')
    if drain is not None:
        low, high = drain['interval_ms']
        offs = [pair['off_ms'] for pair in drain['pairs']]
        spans = [pair['spans_ms'] for pair in drain['pairs']]
        lines.append(
            f"| drain latency, `{drain['fixture']}` | {len(drain['pairs'])} | | "
            f'{statistics.median(offs):.3f} ms | {statistics.median(spans):.3f} ms | '
            f"{drain['median_ms']:+.3f} ms | [{low:+.3f} ms, {high:+.3f} ms] | "
            f"{'pass' if drain['passed'] else 'fail'} |")
    protocol = document['protocol']
    lines += ['', f"Bounds: median overhead at most {protocol['median_bound_pct']}%, interval "
              f"upper bound at most {protocol['upper_bound_pct']}%, drain latency median "
              f'difference at most {DRAIN_BOUND_MS:g} ms.',
              f"Gate: {document['gate']['verdict']}"]
    lines += [f'- {reason}' for reason in document['gate']['reasons']]
    return '\n'.join(lines) + '\n'


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    parser.add_argument('--af', default=str(REPOSITORY / 'target' / 'release' / 'af'),
                        help='the af binary to measure (default: target/release/af)')
    parser.add_argument('--pairs', type=int, default=DEFAULT_PAIRS)
    parser.add_argument('--fixture', action='append', choices=sorted(FIXTURES),
                        help='measure only this fixture (repeatable); the gate needs all three')
    parser.add_argument('--scale', choices=('full', 'smoke'), default='full')
    parser.add_argument('--seed', type=int, default=DEFAULT_SEED)
    parser.add_argument('--work', help='the work directory (default: a fresh temporary one)')
    parser.add_argument('--keep', action='store_true', help='keep every run directory')
    parser.add_argument('--fixture-contract', choices=(CURRENT_CONTRACT,),
                        help='require the current fixture contract; the measured '
                        "binary must admit its exact workload")
    parser.add_argument('--json', action='store_true', help=f'print the {SCHEMA} document')
    parser.add_argument('--report', help='also write the document to this file')
    parser.add_argument('--judge', metavar='DOCUMENT',
                        help='recompute the figures and the verdict of a recorded document')
    args = parser.parse_args(argv)
    if args.pairs < 1:
        parser.error('--pairs must be at least 1')
    # A terminated measurement still removes its temporary work directory.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        document = judge(args.judge) if args.judge else measure(args)
    except BenchError as error:
        print(f'telemetry-overhead: {error}', file=sys.stderr)
        return 1
    if args.report:
        Path(args.report).write_text(json.dumps(document, indent=2) + '\n', encoding='utf-8')
    sys.stdout.write(json.dumps(document, indent=2) + '\n' if args.json else table(document))
    return 3 if document['gate']['verdict'] == 'fail' else 0


if __name__ == '__main__':
    sys.exit(main())
