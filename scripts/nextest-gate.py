#!/usr/bin/env python3
"""Keep nextest reports outside sealed sources when verify.sh selects a target."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

REPORT = Path(__file__).resolve().parent / 'test-time-report.py'


def profile(arguments):
    for i, argument in enumerate(arguments):
        if argument in ('--profile', '-P') and i + 1 < len(arguments):
            return arguments[i + 1]
        if argument.startswith('--profile='):
            return argument.split('=', 1)[1]
    return 'default'


def report(junit, started):
    """Print the slow-test summary of the run that just ended. It never changes the run's
    exit status: a missing, stale or unreadable JUnit is a warning."""
    try:
        if not junit.is_file():
            raise RuntimeError(f'nextest wrote no JUnit at {junit}')
        # A report older than this run belongs to an earlier one (coarse mtime allowance).
        if junit.stat().st_mtime < started - 2:
            raise RuntimeError(f'{junit} predates this run')
        result = subprocess.run([sys.executable, str(REPORT), 'summary', str(junit)],
                                capture_output=True, text=True, timeout=120)
        if result.returncode != 0:
            raise RuntimeError(result.stderr.strip() or f'exit {result.returncode}')
        print(result.stdout, end='', flush=True)
        if summary := os.environ.get('GITHUB_STEP_SUMMARY'):
            with Path(summary).open('a') as stream:
                stream.write(result.stdout)
    except Exception as error:  # Whatever went wrong, the suite's status stands.
        print(f'warning: no test-time report: {error}', file=sys.stderr, flush=True)


def main():
    arguments = sys.argv[1:]
    command = ['cargo', 'nextest', 'run', *arguments]
    junit = Path(profile(arguments)) / 'junit.xml'
    target = os.environ.get('AF_GATE_NEXTEST_TARGET')
    started = time.time()
    if not target:
        # The default store is workspace-relative; make runs from the workspace root.
        code = subprocess.call(command)
        report(Path('target/nextest') / junit, started)
        return code
    # A unique private directory avoids concurrent report/config collisions; verify.sh names
    # one per gate so its step timings sit beside the store. Reports survive failure for
    # diagnosis; only the temporary tool configuration is removed.
    reports = Path(target).resolve() / 'nextest-reports'
    reports.mkdir(parents=True, exist_ok=True)
    if named := os.environ.get('AF_GATE_NEXTEST_RUN'):
        run = Path(named).resolve()
        run.mkdir(parents=True, exist_ok=True)
    else:
        run = Path(tempfile.mkdtemp(prefix='run-', dir=reports))
    config = run / 'store.toml'
    try:
        config.write_text('[store]' + chr(10) + 'dir = ' + json.dumps(str(run / 'store')) + chr(10))
        print(f'nextest gate reports: {run / "store"}', flush=True)
        code = subprocess.call(command + ['--tool-config-file', f'af-gate:{config}'])
    finally:
        config.unlink(missing_ok=True)
    report(run / 'store' / junit, started)
    return code


if __name__ == '__main__':
    code = main()
    sys.exit(code if code >= 0 else 128 - code)
