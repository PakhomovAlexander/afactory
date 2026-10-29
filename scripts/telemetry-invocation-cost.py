#!/usr/bin/env python3
"""The invocation-cost gate of design §12: Claude `json` against `stream-json`, at `off` and `spans`.

    telemetry-invocation-cost.py [--af PATH] [--pairs N] [--scale full|smoke] [--seed N]
                                 [--work DIR] [--keep] [--json] [--report PATH]
    telemetry-invocation-cost.py --judge DOCUMENT [--json]

The protocol is the overhead gate's (`scripts/telemetry-overhead.py`, whose functions it uses):
each measurement is a pair of commands on the same machine, fixture and level, one whose
scripted Claude harness prints the `json` shape and one that prints `stream-json`, in an order
drawn at random per pair (`--seed`), 20 pairs per case by default, every command in a fresh copy
of its fixture with private homes, Store and Provider registry, its wall time from exec to exit.
A case is a fixture — `telemetry-bench-invocation-one` (one Attempt) or
`telemetry-bench-invocation-four` (four concurrent Attempts) — with 1 MiB, 16 MiB or 64 MiB of
intermediate tool output per Attempt (`--scale smoke`: 64 KiB); the harness
(`fixtures/telemetry-bench/invocation-claude.py`) builds the same tool output and answers the
same final result under both shapes and prints only the result under `json`. Per run it records
the command's wall time, its peak RSS (`wait4`, the command with its reaped children), the drain
latency of the one-Attempt fixture (from the harness's last write to the command's observed
exit, both on `CLOCK_MONOTONIC`, as the overhead gate takes it; a negative one is kept as
measured and leaves the gate `not_judged`) and the bytes the harness wrote to stdout; and it requires
the run to finish with the same accounting under both shapes and at both levels (the Task's
charge, its Attempts and every Attempt's usage records), stopping the measurement otherwise.
Each release runs the workload its fixture contract describes (`--fixture-contract`, as the
overhead gate selects it).

The gate is judged at `off`: in every case the median paired wall difference `(stream-json -
json) / json` is at most 2%, and the median paired peak-RSS difference at most the stream's bytes
(the case's median stdout bytes of a `stream-json` run) plus 16 MiB. It is judged only over every
case at `--scale full` with at least 20 pairs; anything less is `not_judged`. A failed gate keeps
two invocation shapes — `json` at `off` and `metrics`, `stream-json` at `spans` and `content` —
and a passed one a single `stream-json` shape (`decision`). `--judge` recomputes every figure
and the verdict from a recorded document's own pairs. Exit status: 0 for `pass` or `not_judged`,
3 for `fail`, 1 when a measurement or a document cannot be read.

Prints a Markdown table, or with `--json` the `af/telemetry-invocation-cost@2` document
(`schemas/telemetry-invocation-cost-v2.json`); `--report PATH` writes the document as well.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import random
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time

REPOSITORY = Path(__file__).resolve().parent.parent
# The overhead script is loaded as a module; no bytecode cache is written beside it.
sys.dont_write_bytecode = True
_SPEC = importlib.util.spec_from_file_location('telemetry_overhead',
                                               REPOSITORY / 'scripts' / 'telemetry-overhead.py')
overhead = importlib.util.module_from_spec(_SPEC)
# Execute the retained source bytes, never a bytecode cache outside authority identity.
_OVERHEAD_PATH = REPOSITORY / 'scripts' / 'telemetry-overhead.py'
exec(compile(_OVERHEAD_PATH.read_bytes(), str(_OVERHEAD_PATH), 'exec'), overhead.__dict__)
BenchError = overhead.BenchError

SCHEMA = 'af/telemetry-invocation-cost@2'
FIXTURES_DIR = overhead.FIXTURES_DIR
HARNESS = FIXTURES_DIR / 'invocation-claude.py'
FIXTURES = {'telemetry-bench-invocation-one': 1, 'telemetry-bench-invocation-four': 4}
SIZES = {'full': [1 << 20, 16 << 20, 64 << 20], 'smoke': [64 << 10]}
CHUNK_BYTES = 64 << 10
SHAPES = ('json', 'stream-json')
LEVELS = ('off', 'spans')
WALL_BOUND_PCT = 2.0
RSS_SLACK_BYTES = 16 << 20
PROVIDER = 'claude-bench'
LAST_WRITE = b'"subtype":"af_bench_last_write"'


def label(size):
    return f'{size >> 20} MiB' if size >= 1 << 20 else f'{size >> 10} KiB'


# ------------------------------------------------------------------------------------------
# the rule


def summarize(document):
    """Recompute every difference, median, interval and the verdict from the recorded pairs."""
    protocol = document['protocol']
    if ((protocol['wall_bound_pct'], protocol['rss_slack_bytes'], protocol['resamples'])
            != (WALL_BOUND_PCT, RSS_SLACK_BYTES, overhead.RESAMPLES)):
        raise BenchError('the document names other bounds or resamples than the protocol')
    if (document['drain']['boundary'], document['drain']['clock']) != (
            overhead.DRAIN_BOUNDARY, overhead.DRAIN_CLOCK):
        raise BenchError('the document measures its drain between other boundaries than the '
                         f'protocol ({overhead.DRAIN_BOUNDARY} on the {overhead.DRAIN_CLOCK} '
                         'clock)')
    contract = document['fixture_contract']
    if overhead.select_contract(contract['release']) != contract['name']:
        raise BenchError(f"the document measured af {contract['release']} under the fixture "
                         f"contract `{contract['name']}`, which does not describe it")
    seed = protocol['seed']
    reasons = []
    for case in document['cases']:
        for level in LEVELS:
            entry = case['levels'][level]
            walls, rss, drains, streams = [], [], [], []
            for pair in entry['pairs']:
                plain, stream = pair['json'], pair['stream-json']
                if plain['wall_ms'] <= 0:
                    raise BenchError(f"{case['name']} {level}: a `json` wall time of "
                                     f"{plain['wall_ms']} ms")
                pair['wall_difference_pct'] = round(
                    (stream['wall_ms'] - plain['wall_ms']) / plain['wall_ms'] * 100, 3)
                pair['rss_difference_bytes'] = stream['peak_rss_bytes'] - plain['peak_rss_bytes']
                walls.append(pair['wall_difference_pct'])
                rss.append(pair['rss_difference_bytes'])
                streams.append(stream['stdout_bytes'])
                if plain['drain_ms'] is not None and stream['drain_ms'] is not None:
                    drains.append(round(stream['drain_ms'] - plain['drain_ms'], 3))
            if not walls:
                raise BenchError(f"{case['name']} {level}: no pairs")
            entry['wall_median_pct'] = round(statistics.median(walls), 3)
            entry['wall_interval_pct'] = overhead.bootstrap_interval(
                walls, seed, f"{case['name']}:{level}")
            entry['rss_median_bytes'] = int(statistics.median(rss))
            entry['stream_bytes'] = int(statistics.median(streams))
            entry['drain_median_ms'] = (round(statistics.median(drains), 3) if drains else None)
            if level == 'off':
                wall_ok = entry['wall_median_pct'] <= WALL_BOUND_PCT
                rss_bound = entry['stream_bytes'] + RSS_SLACK_BYTES
                rss_ok = entry['rss_median_bytes'] <= rss_bound
                entry['passed'] = wall_ok and rss_ok
                if not wall_ok:
                    reasons.append(f"{case['name']}: median wall {entry['wall_median_pct']:+.3f}% "
                                   f"(bound {WALL_BOUND_PCT}%)")
                if not rss_ok:
                    reasons.append(f"{case['name']}: median peak RSS "
                                   f"{entry['rss_median_bytes'] / 2**20:+.1f} MiB (bound the "
                                   f"stream's {entry['stream_bytes'] / 2**20:.1f} MiB plus 16 MiB)")
            else:
                entry['passed'] = None
    # A run's drain is a physical duration: a negative one is an ordering failure of the
    # measurement, kept as measured and never clamped or filtered, and leaves the gate unjudged.
    negative = sum(overhead.negative_drains([pair['json'], pair['stream-json']], ('drain_ms',))
                   for case in document['cases'] for level in LEVELS
                   for pair in case['levels'][level]['pairs'])
    unjudged = []
    if negative:
        unjudged.append(f"drain latency: {negative} negative physical duration"
                        f"{'s' if negative != 1 else ''}, kept as measured and not judged")
    expected = len(FIXTURES) * len(SIZES['full'])
    if protocol['scale'] != 'full':
        unjudged.append(f"scale `{protocol['scale']}`, not `full`")
    if len(document['cases']) != expected:
        unjudged.append(f"{len(document['cases'])} of the {expected} cases measured")
    fewest = min(len(case['levels'][level]['pairs'])
                 for case in document['cases'] for level in LEVELS)
    if fewest < overhead.DEFAULT_PAIRS:
        unjudged.append(f'{fewest} pairs, fewer than {overhead.DEFAULT_PAIRS}')
    if unjudged:
        document['gate'] = {'verdict': 'not_judged', 'reasons': unjudged + reasons}
    elif reasons:
        document['gate'] = {'verdict': 'fail', 'reasons': reasons}
    else:
        document['gate'] = {'verdict': 'pass', 'reasons': []}
    document['decision'] = {'pass': 'one_shape', 'fail': 'two_shapes'}.get(
        document['gate']['verdict'], 'not_judged')
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


def prepare(fixture, work, af, contract):
    """The fixture's repository under the fixture contract `contract`, committed once; every
    run copies it. Returns it with the contract's provenance (`overhead.apply_contract`)."""
    pristine = work / fixture / 'pristine'
    overhead.copy_writable(FIXTURES_DIR / fixture / 'repo', pristine)
    produced = overhead.apply_contract(af, fixture, pristine, contract)
    env = {name: value for name, value in os.environ.items() if not name.startswith('GIT_')}
    env.update(overhead.GIT_IDENTITY)
    for args in (['init', '-q', '-b', 'main'], ['add', '-A'], ['commit', '-qm', fixture]):
        done = subprocess.run(['git', *args], cwd=pristine, env=env, capture_output=True,
                              text=True)
        if done.returncode != 0:
            raise BenchError(f'{fixture}: git {" ".join(args)}: {done.stderr.strip()}')
    return pristine, produced


def objects(state):
    """Every regular object of the run's CAS."""
    root = state / 'cas' / 'objects'
    return [path for path in sorted(root.glob('*/*'))
            if not path.name.startswith('.') and path.is_file()]


def harness_report(state):
    """The harness's last writes and whether the Store holds the drained result.

    Each last write is the harness's reading of `CLOCK_MONOTONIC` after every earlier write
    (`last_write_monotonic_ns`) and its stdout bytes; the drain ends at the measurement's own
    reading of that clock when the command's exit is observed (`overhead.DRAIN_BOUNDARY`).
    """
    lasts, drained = [], False
    for path in objects(state):
        data = path.read_bytes() if path.stat().st_size < 1 << 16 else b''
        if LAST_WRITE in data:
            line = json.loads(data.strip().rsplit(b'\n', 1)[-1])
            if 'last_write_monotonic_ns' not in line:
                raise BenchError(f'{path}: the harness named no last write on the monotonic '
                                 'clock')
            lasts.append((int(line['last_write_monotonic_ns']), int(line['stdout_bytes'])))
        elif data.startswith(b'{"type":"result","subtype":"success"') and b'af.worker-reply' in data:
            drained = True
    return lasts, drained


def accounting(stdout):
    """What the run's accounting and classification came to, from its inspection document."""
    document = json.loads(stdout)
    usage = sorted(
        (record['record']['kind'], record['record'].get('charged_tokens'))
        for record in document['execution_records']
        if record['record']['kind'] in ('usage_observed', 'settled', 'released', 'failed'))
    return {'phase': document['phase']['kind'], 'chargeable_tokens': document['chargeable_tokens'],
            'attempts': document['attempts'], 'usage': usage}


def run_once(af, fixture, size, pristine, work, name, shape, level):
    run = work / fixture / name
    repo = run / 'repo'
    overhead.copy_writable(pristine, repo)
    for directory in ('home', 'config', 'xstate', 'data', 'cache', 'bin', 'tmp', 'auth'):
        (run / directory).mkdir(parents=True)
    os.chmod(run / 'auth', 0o700)
    shutil.copy(HARNESS, run / 'bin' / 'claude')
    os.chmod(run / 'bin' / 'claude', 0o755)
    (run / 'auth' / 'bench.json').write_text(json.dumps({'harness': {
        'shape': shape, 'tool_output_bytes': size, 'chunk_bytes': CHUNK_BYTES}}) + '\n')
    (run / 'providers.toml').write_text(
        f'version = 1\n\n[[providers]]\nid = "{PROVIDER}"\nkind = "claude"\n'
        f'auth_dir = "{run / "auth"}"\n')
    env = overhead.private_env(run, level)
    env['PATH'] = f"{run / 'bin'}{os.pathsep}{env.get('PATH', '/usr/bin:/bin')}"
    env['AF_PROVIDERS_FILE'] = str(run / 'providers.toml')
    env.setdefault('USER', 'bench')
    state = run / 'store'
    argv = [af, 'task', 'start', '--execute', '--file', str(FIXTURES_DIR / fixture / 'task.json'),
            '--state', str(state), '--json']
    with open(run / 'stdout', 'wb') as stdout, open(run / 'stderr', 'wb') as stderr:
        start = time.perf_counter_ns()
        process = subprocess.Popen(argv, cwd=repo, env=env, stdin=subprocess.DEVNULL,
                                   stdout=stdout, stderr=stderr)
        _, status, usage = os.wait4(process.pid, 0)
        exit_observed_ns = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
        wall_ns = time.perf_counter_ns() - start
    code = os.waitstatus_to_exitcode(status)
    if code != 0:
        tail = (run / 'stderr').read_text(encoding='utf-8', errors='replace')[-2000:]
        raise BenchError(f'{fixture} {name}: exit {code}\n{tail}')
    sealed = overhead.segments(run)
    if level == 'off' and sealed is not None:
        raise BenchError(f'{fixture} {name}: a telemetry directory at `off`')
    if level != 'off' and not sealed:
        raise BenchError(f'{fixture} {name}: no telemetry segment at `{level}`')
    lasts, drained = harness_report(state)
    if len(lasts) != FIXTURES[fixture]:
        raise BenchError(f'{fixture} {name}: {len(lasts)} harness reports for '
                         f'{FIXTURES[fixture]} Attempts')
    drain = None
    if FIXTURES[fixture] == 1 and drained:
        drain = overhead.drain_latency(lasts[0][0], exit_observed_ns)
    scale = 1 if sys.platform == 'darwin' else 1024
    result = {'wall_ms': round(wall_ns / 1e6, 3), 'exit_code': code,
              'peak_rss_bytes': usage.ru_maxrss * scale, 'segments': sealed or 0,
              'drain_ms': drain, 'stdout_bytes': sum(bytes_ for _, bytes_ in lasts)}
    return result, accounting((run / 'stdout').read_bytes())


def measure(args):
    af = str(Path(args.af).resolve())
    work = Path(args.work).resolve() if args.work else Path(
        tempfile.mkdtemp(prefix='af-telemetry-invocation-'))
    work.mkdir(parents=True, exist_ok=True)
    version = subprocess.run([af, '--version'], capture_output=True, text=True,
                             env=overhead.private_env(work / 'version', 'off'))
    if version.returncode != 0:
        raise BenchError(f'{af} --version: {version.stderr.strip()}')
    release = overhead.release_version(version.stdout.strip())
    contract = overhead.select_contract(release, args.fixture_contract)
    order_rng = random.Random(args.seed)
    load = os.getloadavg()[0] if hasattr(os, 'getloadavg') else None
    document = {
        'schema': SCHEMA,
        'af': {'path': af, 'version': version.stdout.strip()},
        'fixture_contract': {'name': contract, 'release': release,
                             'check_receipt': overhead.FIXTURE_CONTRACTS[contract]['check_receipt'],
                             'fixtures': []},
        'drain': {'boundary': overhead.DRAIN_BOUNDARY, 'clock': overhead.DRAIN_CLOCK},
        'machine': {'os': overhead.platform.system().lower(), 'arch': overhead.platform.machine(),
                    'cpus': os.cpu_count() or 0,
                    'load1': None if load is None else round(load, 2)},
        'protocol': {'pairs': args.pairs, 'scale': args.scale, 'seed': args.seed,
                     'resamples': overhead.RESAMPLES, 'wall_bound_pct': WALL_BOUND_PCT,
                     'rss_slack_bytes': RSS_SLACK_BYTES},
        'cases': [],
    }
    try:
        for fixture, attempts in FIXTURES.items():
            pristine, produced = prepare(fixture, work, af, contract)
            document['fixture_contract']['fixtures'].append(produced)
            for size in SIZES[args.scale]:
                case = {'name': f'{fixture} {label(size)}', 'fixture': fixture,
                        'attempts': attempts, 'tool_output_bytes': size,
                        'levels': {level: {'pairs': []} for level in LEVELS}}
                expected = None
                for index in range(args.pairs):
                    for level in LEVELS:
                        order = list(SHAPES)
                        order_rng.shuffle(order)
                        pair = {'order': order}
                        for shape in order:
                            name = f'{size}-{index:02d}-{level}-{shape}'
                            pair[shape], counted = run_once(af, fixture, size, pristine, work,
                                                            name, shape, level)
                            if expected is None:
                                expected = counted
                            elif counted != expected:
                                raise BenchError(
                                    f"{case['name']} {name}: accounting {counted}, other runs "
                                    f'{expected}')
                            if not args.keep:
                                shutil.rmtree(work / fixture / name, ignore_errors=True)
                        case['levels'][level]['pairs'].append(pair)
                        print(f"{case['name']} {level}: pair {index + 1}/{args.pairs} json "
                              f"{pair['json']['wall_ms']:.0f} ms, stream-json "
                              f"{pair['stream-json']['wall_ms']:.0f} ms", file=sys.stderr)
                case['accounting'] = expected
                document['cases'].append(case)
    finally:
        if not args.keep and not args.work:
            shutil.rmtree(work, ignore_errors=True)
    return summarize(document)


# ------------------------------------------------------------------------------------------
# output


def table(document):
    lines = ['| Case | Level | Pairs | Median `json` | Median `stream-json` | Median wall | '
             '95% interval | Median peak RSS | Stream bytes | Drain | Verdict |',
             '| --- | --- | ---: | ---: | ---: | ---: | --- | ---: | ---: | ---: | --- |']
    for case in document['cases']:
        for level in LEVELS:
            entry = case['levels'][level]
            plain = statistics.median(pair['json']['wall_ms'] for pair in entry['pairs'])
            stream = statistics.median(pair['stream-json']['wall_ms'] for pair in entry['pairs'])
            low, high = entry['wall_interval_pct']
            drain = ('' if entry['drain_median_ms'] is None
                     else f"{entry['drain_median_ms']:+.3f} ms")
            verdict = {True: 'pass', False: 'fail', None: ''}[entry['passed']]
            lines.append(
                f"| `{case['name']}` | `{level}` | {len(entry['pairs'])} | {plain:.0f} ms | "
                f"{stream:.0f} ms | {entry['wall_median_pct']:+.2f}% | [{low:+.2f}%, {high:+.2f}%] | "
                f"{entry['rss_median_bytes'] / 2**20:+.1f} MiB | "
                f"{entry['stream_bytes'] / 2**20:.1f} MiB | {drain} | {verdict} |")
    lines += ['', f'Gate at `off`: median wall difference at most {WALL_BOUND_PCT}%, median peak '
              'RSS difference at most the stream\'s bytes plus 16 MiB.',
              f"Gate: {document['gate']['verdict']}; decision: {document['decision']}"]
    lines += [f'- {reason}' for reason in document['gate']['reasons']]
    return '\n'.join(lines) + '\n'


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    parser.add_argument('--af', default=str(REPOSITORY / 'target' / 'release' / 'af'),
                        help='the af binary to measure (default: target/release/af)')
    parser.add_argument('--pairs', type=int, default=overhead.DEFAULT_PAIRS)
    parser.add_argument('--scale', choices=('full', 'smoke'), default='full')
    parser.add_argument('--seed', type=int, default=overhead.DEFAULT_SEED)
    parser.add_argument('--work', help='the work directory (default: a fresh temporary one)')
    parser.add_argument('--keep', action='store_true', help='keep every run directory')
    parser.add_argument('--fixture-contract', choices=sorted(overhead.FIXTURE_CONTRACTS),
                        help='require this fixture contract; refused unless the measured '
                        "release's version selects it")
    parser.add_argument('--json', action='store_true', help=f'print the {SCHEMA} document')
    parser.add_argument('--report', help='also write the document to this file')
    parser.add_argument('--judge', metavar='DOCUMENT',
                        help='recompute the figures and the verdict of a recorded document')
    args = parser.parse_args(argv)
    if args.pairs < 1:
        parser.error('--pairs must be at least 1')
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        document = judge(args.judge) if args.judge else measure(args)
    except BenchError as error:
        print(f'telemetry-invocation-cost: {error}', file=sys.stderr)
        return 1
    if args.report:
        Path(args.report).write_text(json.dumps(document, indent=2) + '\n', encoding='utf-8')
    sys.stdout.write(json.dumps(document, indent=2) + '\n' if args.json else table(document))
    return 3 if document['gate']['verdict'] == 'fail' else 0


if __name__ == '__main__':
    sys.exit(main())
