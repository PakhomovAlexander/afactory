#!/usr/bin/env python3
"""Give every nextest run a private report store, outside sealed sources when verify.sh selects
a target, and report the JUnit that run wrote."""
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

REPORT = Path(__file__).resolve().parent / 'test-time-report.py'


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


def main():
    arguments = sys.argv[1:]
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
        code, run, junit = run_nextest(command, reports, None, junit, announce=False)
        if settle(junit, shared, lock):
            shutil.rmtree(run, ignore_errors=True)
        return code
    # A unique private directory avoids concurrent report/config collisions; verify.sh names
    # one per gate so its step timings sit beside the store. Reports survive failure for
    # diagnosis; only the temporary tool configuration is removed.
    code, _, _ = run_nextest(command, Path(target).resolve() / 'nextest-reports',
                             os.environ.get('AF_GATE_NEXTEST_RUN'), junit, announce=True)
    return code


if __name__ == '__main__':
    code = main()
    sys.exit(code if code >= 0 else 128 - code)
