#!/usr/bin/env python3
"""Give every nextest run a private report store, outside sealed sources when verify.sh selects
a target, and report the JUnit that run wrote.

    nextest-gate.py [--repeat N] NEXTEST_RUN_ARGUMENTS...

A gate run reruns the tests that failed in its parallel run once each, alone, before it fails
(ADR-0124): one at a time, with no other test alive. A test that passes alone is reported
`flaky` with both outcomes and does not fail the gate; one that fails both times fails it as
before. The rerun is bounded: more than 25 failed tests, or 15 minutes of reruns, and the gate
fails with the whole list. When `AF_CHECK_REPORT` names a file, as a Task check sets it, the run
appends one `af.check-report/1` tests line to it: the thread count, what failed and what was
flaky.

`--repeat N` runs the same selection N times in a row instead, each a whole parallel run, and
stops at the first that fails; it never reruns anything, because its purpose is to find the
flakes the rerun would forgive (`make test-repeat`). It is an argument only, never read from the
environment, so no variable left in a shell changes what `make check` runs."""
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import xml.etree.ElementTree as ET

REPORT = Path(__file__).resolve().parent / 'test-time-report.py'
# nextest's exit status when tests failed and nothing else did: only then is there a failed set
# to rerun. Any other status (a build that failed, say) stands as it is.
TESTS_FAILED = 100
RERUN_MAX_TESTS = 25
RERUN_MAX_SECONDS = 15 * 60
# Options that select or schedule tests: an isolated rerun replaces them with its own.
SELECTION_OPTIONS = ('--test-threads', '-j', '-E', '--filterset', '--filter-expr', '--partition')


def profile(arguments):
    for i, argument in enumerate(arguments):
        if argument in ('--profile', '-P') and i + 1 < len(arguments):
            return arguments[i + 1]
        if argument.startswith('--profile='):
            return argument.split('=', 1)[1]
    return 'default'


def clear(junit):
    """Remove the JUnit this run will write, so an earlier run's report is never this one's.
    Returns why it could not be removed, or None."""
    try:
        junit.unlink()
    except FileNotFoundError:
        pass
    except OSError as error:
        return f'cannot remove the earlier JUnit at {junit}: {error}'
    return None


def warn(message):
    try:
        print(f'warning: {message}', file=sys.stderr, flush=True)
    except BaseException:
        pass


def report(junit, stale):
    """Print the slow-test summary of the run that just ended. It never changes the run's
    exit status: a missing, earlier or unreadable JUnit is a warning."""
    try:
        if stale:
            raise RuntimeError(stale)
        if not junit.is_file():
            raise RuntimeError(f'nextest wrote no JUnit at {junit}')
        result = subprocess.run([sys.executable, str(REPORT), 'summary', str(junit)],
                                capture_output=True, text=True, timeout=120)
        if result.returncode != 0:
            raise RuntimeError(result.stderr.strip() or f'exit {result.returncode}')
        print(result.stdout, end='', flush=True)
        summary = os.environ.get('GITHUB_STEP_SUMMARY')
        if summary:
            with Path(summary).open('a') as stream:
                stream.write(result.stdout)
    except BaseException as error:  # Whatever went wrong, the suite's status stands.
        warn(f'no test-time report: {error}')


@contextmanager
def locked(lock):
    """Hold an exclusive lock on `lock` while the shared JUnit changes state, so concurrent runs
    clear, publish or retract it one at a time. The kernel drops it if this process dies."""
    lock.parent.mkdir(parents=True, exist_ok=True)
    with lock.open('a') as stream:
        fcntl.flock(stream, fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(stream, fcntl.LOCK_UN)


def publish(junit, path):
    """Copy this run's JUnit to the shared path CI uploads, replacing it in one rename so a
    reader never sees half of it. Returns why it could not, or None."""
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        handle, partial = tempfile.mkstemp(prefix='.junit-', dir=path.parent)
        os.close(handle)
        try:
            shutil.copyfile(junit, partial)
            os.replace(partial, path)
        finally:
            Path(partial).unlink(missing_ok=True)
    except Exception as error:
        return f'cannot publish this run\'s JUnit to {path}: {error}'
    return None


def settle(junit, shared, lock):
    """Leave the shared path in this run's final state under the lock: its JUnit, or nothing if
    it wrote none or the copy failed. The run that settles last decides what CI uploads.
    Returns True when the private JUnit is no longer needed."""
    try:
        with locked(lock):
            published = publish(junit, shared) if junit.is_file() else None
            retracted = clear(shared) if published or not junit.is_file() else None
    except Exception as error:
        warn(f'cannot settle {shared}: {error}; this run\'s JUnit stays at {junit}')
        return False
    if retracted:
        warn(retracted)
    if published:
        warn(f'{published}; it stays at {junit}')
    return not published


def run_nextest(command, reports, named, junit, announce):
    """Run nextest with a store in a private run directory under `reports` (or `named`), so
    concurrent runs never read or clear each other's JUnit, and print its summary; `announce`
    names the store first. Returns the exit status, the run directory and the run's JUnit."""
    reports.mkdir(parents=True, exist_ok=True)
    if named:
        run = Path(named).resolve()
        run.mkdir(parents=True, exist_ok=True)
    else:
        run = Path(tempfile.mkdtemp(prefix='run-', dir=reports))
    junit = run / 'store' / junit
    stale = clear(junit)
    config = run / 'store.toml'
    try:
        config.write_text('[store]' + chr(10) + 'dir = ' + json.dumps(str(run / 'store')) + chr(10))
        if announce:
            print(f'nextest gate reports: {run / "store"}', flush=True)
        code = subprocess.call(command + ['--tool-config-file', f'af-gate:{config}'])
    finally:
        config.unlink(missing_ok=True)
    report(junit, stale)
    return code, run, junit


def own_options(arguments):
    """(repeat, nextest arguments): this script's `--repeat N`, else None, and everything else
    for nextest."""
    repeat, rest, index = None, [], 0
    while index < len(arguments):
        argument = arguments[index]
        if argument == '--repeat' and index + 1 < len(arguments):
            repeat, index = arguments[index + 1], index + 2
            continue
        if argument.startswith('--repeat='):
            repeat, index = argument.split('=', 1)[1], index + 1
            continue
        rest.append(argument)
        index += 1
    if repeat is not None:
        if not repeat.isdigit() or int(repeat) < 1:
            raise SystemExit(f'nextest-gate.py: --repeat takes a whole number of runs, not '
                             f'{repeat!r}')
        repeat = int(repeat)
    return repeat, rest


def test_threads(arguments):
    """The thread count the run was given, or None when nextest chooses it."""
    for index, argument in enumerate(arguments):
        if argument in ('--test-threads', '-j') and index + 1 < len(arguments):
            value = arguments[index + 1]
        elif argument.startswith('--test-threads='):
            value = argument.split('=', 1)[1]
        else:
            continue
        return int(value) if value.isdigit() and int(value) > 0 else None
    return None


def without_selection(arguments):
    """`arguments` without the options that select or schedule tests."""
    kept, index = [], 0
    while index < len(arguments):
        argument = arguments[index]
        if argument in SELECTION_OPTIONS:
            index += 2
            continue
        if any(argument.startswith(option + '=') for option in SELECTION_OPTIONS):
            index += 1
            continue
        kept.append(argument)
        index += 1
    return kept


def failed_tests(junit):
    """Every (binary, test) the JUnit records as failed, or None when it cannot be read."""
    try:
        root = ET.parse(junit).getroot()
    except (OSError, ET.ParseError):
        return None
    failed = []
    for suite in root.iter('testsuite'):
        for case in suite.iter('testcase'):
            if case.find('failure') is not None or case.find('error') is not None:
                failed.append((case.get('classname') or suite.get('name') or '',
                               case.get('name') or ''))
    return failed


def test_name(test):
    binary, name = test
    return f'{binary} {name}'


def rerun_alone(failed, run_one, clock=time.monotonic, max_tests=RERUN_MAX_TESTS,
                max_seconds=RERUN_MAX_SECONDS):
    """(flaky, failed again or never rerun, rerun state): run each failed test again on its own,
    in order, through `run_one(test, seconds_left)`, which says whether it passed. Above
    `max_tests` nothing reruns; past `max_seconds` the rest does not, and either way the gate
    fails with every test not shown flaky (`over_bound`)."""
    if len(failed) > max_tests:
        return [], list(failed), 'over_bound'
    started = clock()
    flaky, again = [], []
    for index, test in enumerate(failed):
        left = max_seconds - (clock() - started)
        if left <= 0:
            return flaky, again + list(failed[index:]), 'over_bound'
        (flaky if run_one(test, left) else again).append(test)
    return flaky, again, 'isolated'


def run_supervised(command, seconds):
    """`command`'s exit status, or None when it ran past `seconds` and its process group was
    ended."""
    process = subprocess.Popen(command, start_new_session=True)
    try:
        return process.wait(timeout=seconds)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()
        return None


def isolated_runner(arguments, run):
    """`run_one` for `rerun_alone`: one nextest run of exactly `test`, one thread, with its own
    report store below `run`. It passed only if nextest exited 0 and recorded exactly that one
    test, passing."""
    base = ['cargo', 'nextest', 'run', *without_selection(arguments), '--test-threads', '1']
    junit = Path(profile(arguments)) / 'junit.xml'
    counter = iter(range(1, RERUN_MAX_TESTS + 1))

    def run_one(test, seconds):
        binary, name = test
        directory = run / f'isolated-{next(counter)}'
        store = directory / 'store'
        config = directory / 'store.toml'
        directory.mkdir(parents=True, exist_ok=True)
        config.write_text('[store]' + chr(10) + 'dir = ' + json.dumps(str(store)) + chr(10))
        print(f'isolation rerun: {test_name(test)}', flush=True)
        try:
            code = run_supervised(base + ['-E', f'binary_id(={binary}) & test(={name})',
                                          '--tool-config-file', f'af-gate:{config}'], seconds)
        finally:
            config.unlink(missing_ok=True)
        cases = failed_tests(store / junit)
        try:
            recorded = len(ET.parse(store / junit).getroot().findall('.//testcase'))
        except (OSError, ET.ParseError):
            recorded = 0
        return code == 0 and cases == [] and recorded == 1

    return run_one


def report_tests(threads, failed, flaky, rerun):
    """Append the run's tests line to `AF_CHECK_REPORT`, when af asked for one; a report that
    cannot be written is a warning, never a different status."""
    path = os.environ.get('AF_CHECK_REPORT')
    if not path:
        return
    record = {'schema': 'af.check-report/1', 'record': 'tests',
              'test_threads': threads or os.cpu_count() or 1, 'failed_count': len(failed),
              'failed': [test_name(test) for test in failed[:100]],
              'flaky': [{'name': test_name(test), 'parallel': 'failed', 'isolated': 'passed'}
                        for test in flaky],
              'rerun': rerun}
    try:
        with Path(path).open('a') as stream:
            stream.write(json.dumps(record, sort_keys=True) + chr(10))
    except OSError as error:
        warn(f'cannot append the test run to {path}: {error}')


def settle_failures(code, arguments, run, junit):
    """The gate's status once the tests that failed in parallel (exit `code`) ran again alone,
    with what it printed and reported."""
    threads = test_threads(arguments)
    if code == 0:
        report_tests(threads, [], [], 'not_needed')
        return 0
    failed = failed_tests(junit) if code == TESTS_FAILED else None
    if not failed:
        report_tests(threads, [], [], 'not_run')
        return code
    if len(failed) > RERUN_MAX_TESTS:
        print(f'isolation rerun: {len(failed)} tests failed, more than the {RERUN_MAX_TESTS} '
              f'it reruns; none ran again', flush=True)
    else:
        print(f'isolation rerun: {len(failed)} failed in the parallel run; each runs again '
              f'alone', flush=True)
    flaky, again, rerun = rerun_alone(failed, isolated_runner(arguments, run))
    if flaky:
        print('flaky: ' + ', '.join(test_name(test) for test in flaky), flush=True)
    if again:
        verb = 'failed twice' if rerun == 'isolated' else 'failed'
        print(f'{verb}: ' + ', '.join(test_name(test) for test in again), flush=True)
    if rerun == 'over_bound' and len(failed) <= RERUN_MAX_TESTS:
        print(f'isolation rerun: stopped at its {RERUN_MAX_SECONDS // 60}-minute bound',
              flush=True)
    report_tests(threads, again, flaky, rerun)
    return 0 if not again else code


def repeat_runs(repeat, command, reports, named_run, junit_name, announce):
    """Run nextest `repeat` times in a row, each a whole run with its own store, stopping at the
    first that fails. Returns the status, the last run directory and its JUnit."""
    for index in range(1, repeat + 1):
        print(f'repeat: run {index} of {repeat}', flush=True)
        directory = Path(named_run) / f'repeat-{index}' if named_run else None
        code, run, junit = run_nextest(command, reports, directory, junit_name, announce)
        if code != 0:
            failed = failed_tests(junit) or []
            names = ', '.join(test_name(test) for test in failed) or f'exit {code}'
            print(f'repeat: run {index} of {repeat} failed: {names}', flush=True)
            return code, run, junit
        if not named_run and index < repeat:
            shutil.rmtree(run, ignore_errors=True)
    print(f'repeat: all {repeat} runs passed', flush=True)
    return 0, run, junit


def main():
    repeat, arguments = own_options(sys.argv[1:])
    command = ['cargo', 'nextest', 'run', *arguments]
    junit = Path(profile(arguments)) / 'junit.xml'
    target = os.environ.get('AF_GATE_NEXTEST_TARGET')
    if not target:
        # The default paths are workspace-relative; make runs from the workspace root. The run
        # reports its own JUnit from a private store, then publishes it to nextest's default
        # path, which CI uploads and contributors copy. That path is cleared first, and a run
        # that writes no JUnit clears it again when it ends, so it never holds another run's.
        reports = Path('target/nextest-reports').resolve()
        shared = Path('target/nextest') / junit
        lock = reports / f'{junit.parent}.lock'
        try:
            with locked(lock):
                cleared = clear(shared)
        except Exception as error:
            cleared = f'cannot clear {shared}: {error}'
        if cleared:
            warn(cleared)
        if repeat:
            code, run, junit = repeat_runs(repeat, command, reports, None, junit, False)
        else:
            code, run, junit = run_nextest(command, reports, None, junit, announce=False)
            code = settle_failures(code, arguments, run, junit)
        if settle(junit, shared, lock):
            shutil.rmtree(run, ignore_errors=True)
        return code
    # A unique private directory avoids concurrent report/config collisions; verify.sh names
    # one per gate so its step timings sit beside the store. Reports survive failure for
    # diagnosis; only the temporary tool configuration is removed.
    reports = Path(target).resolve() / 'nextest-reports'
    if repeat:
        code, _, _ = repeat_runs(repeat, command, reports, os.environ.get('AF_GATE_NEXTEST_RUN'),
                                 junit, True)
        return code
    code, run, junit = run_nextest(command, reports, os.environ.get('AF_GATE_NEXTEST_RUN'), junit,
                                   announce=True)
    return settle_failures(code, arguments, run, junit)


if __name__ == '__main__':
    code = main()
    sys.exit(code if code >= 0 else 128 - code)
