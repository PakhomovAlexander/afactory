#!/usr/bin/env python3
"""Prove that the recurring load flakes stay fixed: run the named tests N times under a bounded,
self-cleaning concurrent load and print each test's pass count, the load it ran under and its
wall (issue #331, CONTRIBUTING.md).

    stress-proof.py [--runs N] [--load-workers W] [--spawn-workers S] [--test-threads T]
                    [--test MODULE::NAME]... [--keep DIR]

Each run is one nextest run of every selected test with the gate's own profile and scheduling
(`--profile ci`: no retries, the exclusive override of `.config/nextest.toml`), never stopped
early and never rerun: a run that fails is counted as failed and the next one starts. While the
runs go on, W CPU-bound load workers (one per CPU by default) spin beside them, and S spawn
workers (2 by default) start and reap `/usr/bin/true` as fast as they can, so the tests meet the
CPU contention and the process churn a shared gate host gives them whatever else the machine does:
the churn recycles process IDs within minutes, as a full gate does, which is what exposes a test
that signals a process ID it no longer owns (issue #331 item 7).

The load generator cleans up on every exit path. It starts each worker itself, in a session of
its own, and signals only the pids it started, never a name or a pattern, before it reaps them:
on a normal end, an exception, Ctrl-C, SIGTERM or SIGHUP. A worker also checks every few
milliseconds that this process is still its parent and exits when it is not, so even a SIGKILL
of this process leaves no worker behind. A nextest run interrupted the same way is ended by its
own process group, which this process created.

The 1-minute load average is sampled every second; a test's load range is the range sampled while
it ran (from its JUnit timestamp and time). The JUnit of every run with a failure is kept below
`--keep` (default `target/stress-proof/`) with its failure output, and the exit status is 0 only
when every test passed every run."""
import argparse
from datetime import datetime
import json
import os
from pathlib import Path
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parent.parent
# The recurring families of issue #331 (items 7 and 8), as nextest names them in `af::it`.
FAMILIES = (
    'task_starters::heavy_starter_repair_uses_current_evidence',
    'task_repair::interrupted_fix_verification_resumes_same_continuation_and_charges_the_lost_attempt',
    'campaign_loop::trusted_reuse_keeps_evidence_satisfaction_current_after_head_change',
    'campaign_loop::waiver_remains_current_when_the_subject_advances',
    'telemetry_runs::telemetry_tail_follows_a_live_run_within_the_flush_latency',
    'task_file::captured_task_kind_packages_keep_business_names_and_domain_acceptance',
    'review_plan_refusals::first_run_refusals_name_the_fix',
)
BINARY = 'af::it'
SAMPLE_SECONDS = 1.0
# How long an interrupted nextest gets to end its tests on SIGTERM before its group is killed.
STOP_GRACE_SECONDS = 20
# A CPU worker spins this many iterations between checks that its parent is still this
# process: a few milliseconds of CPU, so an orphan is gone almost at once.
CPU_WORKER = '''
import os, sys
parent = int(sys.argv[1])
while os.getppid() == parent:
    for _ in range(100000):
        pass
'''
# A spawn worker starts `/usr/bin/true` and reaps it, again and again, checking its parent
# between spawns; its own child is reaped before the next check, so it leaves none behind.
# A host out of processes refuses a spawn for a moment: the worker waits and spawns again
# rather than ending the load it is there to give.
SPAWN_WORKER = '''
import os, sys, time
parent = int(sys.argv[1])
while os.getppid() == parent:
    try:
        os.waitpid(os.posix_spawn('/usr/bin/true', ['true'], {}), 0)
    except OSError:
        time.sleep(0.01)
'''


class Load:
    """`cpu` CPU-bound and `spawn` process-spawning workers, started and ended by this process
    alone, by pid. A worker that ended on its own is a load the runs no longer had: `ended()`
    names it, with its exit status and the end of its stderr, kept below `logs`."""

    def __init__(self, cpu, spawn=0, logs=None, programs=None):
        self.programs = programs or [CPU_WORKER] * cpu + [SPAWN_WORKER] * spawn
        self.logs = Path(logs) if logs is not None else None
        self.workers = []
        self.stderr = []

    def start(self):
        for index, program in enumerate(self.programs):
            stderr = subprocess.DEVNULL
            if self.logs is not None:
                stderr = open(self.logs / f'load-worker-{index}.stderr', 'w+b')
                self.stderr.append(stderr)
            self.workers.append(subprocess.Popen(
                [sys.executable, '-I', '-c', program, str(os.getpid())],
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=stderr,
                start_new_session=True))
        return self

    def ended(self):
        """Every worker that is no longer running though this process did not stop it, as
        `(pid, exit status, last stderr lines)`. `poll` reaps it, so its pid is not signalled
        afterwards."""
        ended = []
        for index, worker in enumerate(self.workers):
            if worker.poll() is None:
                continue
            tail = ''
            if index < len(self.stderr):
                self.stderr[index].seek(0)
                tail = self.stderr[index].read()[-2000:].decode('utf-8', 'replace').strip()
            ended.append((worker.pid, worker.returncode, tail))
        return ended

    def stop(self):
        """Kill and reap every worker this process started. A worker is this process's
        unreaped child until the `wait` below, so its pid names only it when it is signalled.
        Returns the workers that had ended on their own before (`ended()`)."""
        ended = self.ended()
        workers, self.workers = self.workers, []
        for worker in workers:
            if worker.returncode is None:
                try:
                    os.kill(worker.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
        for worker in workers:
            worker.wait()
        for stream in self.stderr:
            stream.close()
        self.stderr = []
        return ended

    def __enter__(self):
        return self.start()

    def __exit__(self, *_):
        self.stop()


class Sampler:
    """The 1-minute load average, sampled every `period` seconds on a thread of its own."""

    def __init__(self, period=SAMPLE_SECONDS, clock=time.time, load=os.getloadavg):
        self.period, self.clock, self.load = period, clock, load
        self.samples = []
        self.stopped = threading.Event()
        self.thread = threading.Thread(target=self._run, daemon=True)

    def _run(self):
        while True:
            self.samples.append((self.clock(), self.load()[0]))
            if self.stopped.wait(self.period):
                return

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.stopped.set()
        self.thread.join()

    def between(self, start, end):
        """(lowest, highest) load sampled from `start` to `end`, else the nearest sample."""
        inside = [load for at, load in self.samples if start <= at <= end]
        if not inside and self.samples:
            nearest = min(self.samples, key=lambda sample: abs(sample[0] - start))
            inside = [nearest[1]]
        return (min(inside), max(inside)) if inside else None


def interrupt_as_exit(signum, _frame):
    """SIGTERM and SIGHUP end this process the way Ctrl-C does: through every `finally`."""
    raise SystemExit(128 + signum)


def filter_expression(tests):
    return 'package(af) & binary(it) & (' + ' | '.join(f'test(={t})' for t in tests) + ')'


def nextest_command(tests, threads, config):
    return ['cargo', 'nextest', 'run', '--locked', '--profile', 'ci', '--no-fail-fast',
            '--test-threads', str(threads), '-E', filter_expression(tests),
            '--tool-config-file', f'af-gate:{config}']


def run_supervised(command, cwd):
    """Run `command` in a process group of its own; when this process is interrupted, end that
    group (SIGTERM, then SIGKILL after the grace) before the leader is reaped."""
    process = subprocess.Popen(command, cwd=cwd, start_new_session=True)
    try:
        return process.wait()
    except BaseException:
        for sent in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.killpg(process.pid, sent)
            except (ProcessLookupError, PermissionError):
                break
            try:
                process.wait(timeout=STOP_GRACE_SECONDS)
                break
            except subprocess.TimeoutExpired:
                continue
        process.wait()
        raise


def outcomes(junit):
    """{test: (passed, started, seconds, failure text)} for every `af::it` case in `junit`."""
    try:
        root = ET.parse(junit).getroot()
    except (OSError, ET.ParseError):
        return {}
    found = {}
    for case in root.iter('testcase'):
        if case.get('classname') != BINARY:
            continue
        failure = case.find('failure')
        if failure is None:
            failure = case.find('error')
        # A skipped case did not run, so it did not pass.
        skipped = case.find('skipped')
        text = ''
        if failure is not None:
            parts = [failure.get('message') or '', failure.text or '']
            parts += [(case.findtext(name) or '') for name in ('system-out', 'system-err')]
            text = '\n'.join(part for part in parts if part)
        elif skipped is not None:
            text = 'skipped: ' + (skipped.get('message') or skipped.text or 'not run')
        stamp = case.get('timestamp')
        started = datetime.fromisoformat(stamp).timestamp() if stamp else None
        passed = failure is None and skipped is None
        found[case.get('name')] = (passed, started, float(case.get('time') or 0), text)
    return found


def judged(found, tests, code):
    """The outcomes of one run whose nextest exited `code`: a test absent from its JUnit did
    not run, and a run nextest ended nonzero passed nothing, whatever its JUnit says."""
    found = dict(found)
    for test in tests:
        if test not in found:
            found[test] = (False, None, 0.0, f'not run: nextest exit {code}')
        elif code != 0 and found[test][0]:
            passed, started, seconds, text = found[test]
            found[test] = (False, started, seconds, f'nextest exited {code} after this run')
    return found


def one_run(index, tests, threads, keep, run=None):
    """Run every test once; returns its outcomes, keeping the JUnit of a run with a failure."""
    run = run or run_supervised
    with tempfile.TemporaryDirectory(prefix='stress-run-', dir=keep) as directory:
        store = Path(directory) / 'store'
        config = Path(directory) / 'store.toml'
        config.write_text('[store]\ndir = ' + json.dumps(str(store)) + '\n')
        code = run(nextest_command(tests, threads, config), ROOT)
        junit = store / 'ci' / 'junit.xml'
        found = outcomes(junit)
        missing = [test for test in tests if test not in found]
        if code != 0 or missing or not all(found[t][0] for t in found):
            kept = keep / f'run-{index}-junit.xml'
            if junit.is_file():
                shutil.copyfile(junit, kept)
                print(f'stress: run {index} failed; its JUnit is at {kept}', flush=True)
            else:
                print(f'stress: run {index} wrote no JUnit (nextest exit {code})', flush=True)
        return judged(found, tests, code)


def table(tests, results, runs, sampler):
    """The per-test rows: name, passes/runs, load range, wall min/median/max."""
    rows = []
    for test in tests:
        seen = [run[test] for run in results]
        passed = sum(1 for outcome in seen if outcome[0])
        loads = [sampler.between(start, start + seconds)
                 for _, start, seconds, _ in seen if start is not None]
        loads = [load for load in loads if load]
        walls = [seconds for _, start, seconds, _ in seen if start is not None]
        load = (f'{min(low for low, _ in loads):.1f}-{max(high for _, high in loads):.1f}'
                if loads else '-')
        wall = (f'{min(walls):.1f}/{statistics.median(walls):.1f}/{max(walls):.1f}'
                if walls else '-')
        rows.append((test, f'{passed}/{runs}', load, wall))
    return rows


def render(rows):
    header = ('test', 'passes', 'load (1-min avg)', 'wall s min/median/max')
    widths = [max(len(row[i]) for row in [header, *rows]) for i in range(4)]
    line = lambda row: '  '.join(cell.ljust(widths[i]) for i, cell in enumerate(row)).rstrip()
    return '\n'.join([line(header), line(tuple('-' * width for width in widths)),
                      *map(line, rows)])


def main(arguments):
    cpus = os.cpu_count() or 4
    parser = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    parser.add_argument('--runs', type=int, default=20, help='runs of every test (default 20)')
    parser.add_argument('--load-workers', type=int, default=cpus,
                        help=f'CPU-bound load workers beside the tests (default {cpus}, one per '
                             'CPU)')
    parser.add_argument('--spawn-workers', type=int, default=2,
                        help='process-spawning load workers beside the tests (default 2)')
    parser.add_argument('--test-threads', type=int, default=max(4, cpus // 2),
                        help='nextest test threads, as `make test` sets them')
    parser.add_argument('--test', action='append', dest='tests', metavar='MODULE::NAME',
                        help='a test of af::it to run instead of the recurring families '
                             '(repeatable)')
    parser.add_argument('--keep', type=Path, default=ROOT / 'target' / 'stress-proof',
                        help='where the JUnit of a failed run is kept')
    options = parser.parse_args(arguments)
    if (options.runs < 1 or options.test_threads < 1 or options.load_workers < 0
            or options.spawn_workers < 0):
        parser.error('--runs and --test-threads take a whole number above 0, --load-workers '
                     'and --spawn-workers one of 0 or more')
    tests = options.tests or list(FAMILIES)
    keep = options.keep.resolve()
    keep.mkdir(parents=True, exist_ok=True)
    for signum in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, interrupt_as_exit)
    # Build and list first, outside the measured runs and before the load starts: a name that
    # selects nothing is refused here, never counted as a failed run.
    listed = subprocess.run(['cargo', 'nextest', 'list', '--locked', '--message-format', 'oneline',
                             '-E', filter_expression(tests)], cwd=ROOT, stdout=subprocess.PIPE,
                            text=True)
    if listed.returncode != 0:
        print(f'stress: the tests did not build or list (exit {listed.returncode})',
              file=sys.stderr)
        return listed.returncode
    names = {line.split(' ', 1)[1] for line in listed.stdout.splitlines()
             if line.startswith(BINARY + ' ')}
    unknown = [test for test in tests if test not in names]
    if unknown:
        print('stress: no such test in af::it: ' + ', '.join(unknown), file=sys.stderr)
        return 2
    workers = (f'{options.load_workers} CPU and {options.spawn_workers} spawn load workers, '
               f'{options.test_threads} test threads, {cpus} CPUs')
    print(f'stress: {len(tests)} tests, {options.runs} runs, {workers}', flush=True)
    results = []
    started = time.monotonic()
    interrupted = None
    lost = []
    load = Load(options.load_workers, options.spawn_workers, logs=keep)
    with Sampler() as sampler, load:
        try:
            for index in range(1, options.runs + 1):
                # A run counts only while every load worker ran beside it: a worker that ended
                # on its own ends the proof, and the run it ended in is not counted.
                lost = load.ended()
                if lost:
                    break
                # Free space where af puts its sandboxes: below af's floor a run fails on the
                # host, not on the test, and the progress line says so beside the failure.
                free = shutil.disk_usage(tempfile.gettempdir()).free / (1 << 30)
                print(f'stress: run {index} of {options.runs} ({free:.1f} GiB free in '
                      f'{tempfile.gettempdir()})', flush=True)
                results.append(one_run(index, tests, options.test_threads, keep))
                lost = load.ended()
                if lost:
                    results.pop()
                    break
                failed = [test for test in tests if not results[-1][test][0]]
                for test in failed:
                    print(f'stress: run {index} failed: {test}\n'
                          f'{results[-1][test][3][-4000:]}', flush=True)
        except (KeyboardInterrupt, SystemExit) as error:
            interrupted = error
    return summarize(tests, results, options.runs, sampler, started, workers, lost, interrupted)


def summarize(tests, results, wanted, sampler, started, workers, lost, interrupted):
    """Print what the finished runs showed and return the proof's exit status: 0 only when
    every one of the `wanted` runs ran under the whole load and every test passed in each."""
    runs = len(results)
    every = False
    if runs:
        rows = table(tests, results, runs, sampler)
        loads = [load for _, load in sampler.samples]
        print(render(rows))
        if loads:
            print(f'stress: load {min(loads):.1f}-{max(loads):.1f} over '
                  f'{(time.monotonic() - started) / 60:.1f} min, {workers}')
        every = all(row[1] == f'{runs}/{runs}' for row in rows)
    for pid, status, tail in lost:
        print(f'stress: load worker {pid} ended on its own (exit {status}) after {runs} '
              f'counted runs, so the rest did not run under the requested load'
              + (f':\n{tail}' if tail else ''), flush=True)
    if interrupted is not None:
        print(f'stress: interrupted after {runs} of {wanted} runs', flush=True)
        return 130 if isinstance(interrupted, KeyboardInterrupt) else interrupted.code
    if lost:
        print(f'stress: not proven: {runs} of {wanted} runs ran under the whole load')
        return 1
    if runs != wanted:
        print(f'stress: not proven: {runs} of {wanted} runs finished')
        return 1
    print('stress: every test passed every run' if every else 'stress: not every run passed')
    return 0 if every else 1

if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
