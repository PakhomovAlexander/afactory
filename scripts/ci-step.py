#!/usr/bin/env python3
"""Run a gate unchanged and retain elapsed time, including failed invocations."""
import datetime
import json
import os
from pathlib import Path
import subprocess
import sys
import time


def main():
    label, *command = sys.argv[1:]
    if not command:
        raise SystemExit('usage: ci-step.py LABEL COMMAND [ARGS...]')
    started = datetime.datetime.now(datetime.timezone.utc).isoformat()
    clock = time.monotonic()
    try:
        code = subprocess.call(command)
    except FileNotFoundError:
        code = 127
    elapsed = round(time.monotonic() - clock, 3)
    record = {
        'schema': 'af.ci-step/1', 'label': label, 'command': command,
        'started_at': started, 'elapsed_seconds': elapsed, 'exit_code': code,
        'commit': os.getenv('GITHUB_SHA'), 'run_id': os.getenv('GITHUB_RUN_ID'),
        'run_attempt': os.getenv('GITHUB_RUN_ATTEMPT'),
        'runner_os': os.getenv('RUNNER_OS'), 'runner_arch': os.getenv('RUNNER_ARCH'),
        # These timings are neither provider billing nor GitHub runner billing.
        'tokens': None, 'billed_runner_minutes': None,
    }
    if path := os.getenv('AF_CI_METRICS'):
        with Path(path).open('a') as stream:
            stream.write(json.dumps(record, sort_keys=True) + '\n')
    if summary := os.getenv('GITHUB_STEP_SUMMARY'):
        with Path(summary).open('a') as stream:
            stream.write(f'- `{label}`: {elapsed:.3f}s; exit {code}\n')
    print(f'[{label}] {elapsed:.3f}s, exit {code}', flush=True)
    return code if code >= 0 else 128 - code


if __name__ == '__main__':
    sys.exit(main())
