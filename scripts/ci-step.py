#!/usr/bin/env python3
"""Run a gate unchanged and retain elapsed time, including failed invocations.

    ci-step.py [--stage STAGE] [--fix COMMAND] LABEL COMMAND [ARGS...]

`--stage` names where in the gate the step belongs (preflight, lint, build, tests, release) and
`--fix` the one command that repairs what the step refuses. When `AF_CHECK_REPORT` names a file,
as a Task check sets it, the step appends one `af.check-report/1` line to it, so af can say which
step a failed gate stopped at and how to fix it."""
import datetime
import json
import os
from pathlib import Path
import subprocess
import sys
import time

STAGES = ('preflight', 'lint', 'build', 'tests', 'release')
USAGE = 'usage: ci-step.py [--stage STAGE] [--fix COMMAND] LABEL COMMAND [ARGS...]'


def options(arguments):
    """(stage, fix, label, command) from this script's arguments."""
    stage, fix = None, None
    while arguments and arguments[0] in ('--stage', '--fix'):
        if len(arguments) < 2:
            raise SystemExit(USAGE)
        if arguments[0] == '--stage':
            stage = arguments[1]
            if stage not in STAGES:
                raise SystemExit(f'ci-step.py: --stage {stage!r} is not one of {", ".join(STAGES)}')
        else:
            fix = arguments[1]
        arguments = arguments[2:]
    if len(arguments) < 2:
        raise SystemExit(USAGE)
    return stage, fix, arguments[0], arguments[1:]


def report_step(label, stage, fix, command, code):
    """Append this step to the Task check's report, when af asked for one. Never changes the
    step's status: a report that cannot be written is a warning."""
    path = os.getenv('AF_CHECK_REPORT')
    if not path or stage is None:
        return
    record = {'schema': 'af.check-report/1', 'record': 'step', 'label': label, 'stage': stage,
              'exit_code': code, 'command': command}
    if fix:
        record['fix'] = fix
    try:
        with Path(path).open('a') as stream:
            stream.write(json.dumps(record, sort_keys=True) + '\n')
    except OSError as error:
        print(f'warning: cannot append step {label} to {path}: {error}', file=sys.stderr,
              flush=True)


def main():
    stage, fix, label, command = options(sys.argv[1:])
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
    report_step(label, stage, fix, command, code)
    print(f'[{label}] {elapsed:.3f}s, exit {code}', flush=True)
    if code != 0 and stage == 'preflight':
        hint = f'; fix: {fix}' if fix else ''
        print(f'gate stopped at preflight step {label}{hint}', flush=True)
    return code if code >= 0 else 128 - code


if __name__ == '__main__':
    sys.exit(main())
