#!/usr/bin/env python3
"""Run a gate unchanged and retain elapsed time, including failed invocations.

    ci-step.py [--stage STAGE] [--fix COMMAND] LABEL COMMAND [ARGS...]
    ci-step.py --finish-tests PRIVATE_RUNS.jsonl

`--stage` names where in the gate the step belongs (preflight, lint, build, tests, release) and
`--fix` the one command that repairs what the step refuses. When `AF_CHECK_REPORT` names a file,
as a Task check sets it, the step appends one `af.check-report/1` line to it, so af can say which
step a failed gate stopped at and how to fix it. verify.sh uses --finish-tests to fold its
private nextest run records into one tests record after the gate, preserving step streaming."""
import datetime
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import unicodedata

STAGES = ('preflight', 'lint', 'build', 'tests', 'release')
USAGE = 'usage: ci-step.py [--stage STAGE] [--fix COMMAND] LABEL COMMAND [ARGS...]'


def validate_tests(record):
    """Check the unchanged af.check-report/1 tests contract before folding private runs.

    Keep these bounds and rerun rules aligned with CheckTestsV1::validate; the Rust parity
    regression passes this producer's output through the normal Task report parser.
    """
    def integer(value, low, high):
        return type(value) is int and low <= value <= high

    def name(value):
        return (isinstance(value, str) and bool(value.strip()) and len(value) <= 512
                and not any(unicodedata.category(c) == 'Cc' for c in value))

    fields = {'schema', 'record', 'test_threads', 'failed_count', 'failed', 'flaky', 'rerun'}
    if (not isinstance(record, dict) or record.keys() != fields
            or record['schema'] != 'af.check-report/1' or record['record'] != 'tests'
            or not integer(record['test_threads'], 1, 65536)
            or not integer(record['failed_count'], 0, 2 ** 32 - 1)):
        raise ValueError('invalid tests record fields or counts')
    failed, flaky, count = record['failed'], record['flaky'], record['failed_count']
    if (not isinstance(failed, list) or not isinstance(flaky, list)
            or len(failed) > min(100, count) or (count < 100 and len(failed) != count)
            or len(flaky) > 25 or not all(name(n) for n in failed)
            or not all(isinstance(f, dict) and f.keys() == {'name', 'parallel', 'isolated'}
                       and name(f['name']) and f['parallel'] == 'failed'
                       and f['isolated'] == 'passed' for f in flaky)):
        raise ValueError('invalid failed or flaky tests')
    valid_rerun = {'not_needed': count == 0 and not flaky,
                  'not_run': not flaky, 'isolated': count > 0 or bool(flaky),
                  'over_bound': count > 0}
    if not isinstance(record['rerun'], str) or not valid_rerun.get(record['rerun'], False):
        raise ValueError('rerun state disagrees with tests')


def finish_tests(path):
    """Publish one complete tests record for verify.sh, even when several classes ran.

    Steps continue to stream directly to AF_CHECK_REPORT so the build phase monitor sees
    them immediately. Only nextest's tests records use the gate's private spool. Nothing
    changes the record schema or the Task parser, including its bounds and refusal rules.
    """
    if not Path(path).exists():
        return  # A gate that stopped before nextest has no tests record.
    with Path(path).open('rb') as stream:
        data = stream.read(256 * 1024 + 1)
    if len(data) > 256 * 1024:
        raise ValueError('test runs exceed the check report byte bound')
    lines = data.decode('utf-8').splitlines()
    if len(lines) > 256:
        raise ValueError('test runs exceed the check report line bound')
    folded, earlier, flaky_names = None, set(), set()
    capped, summed = False, 0
    precedence = ('not_needed', 'not_run', 'isolated', 'over_bound')

    def unique_fields(pairs):
        value = {}
        for key, entry in pairs:
            if key in value:
                raise ValueError(f'duplicate field {key!r}')
            value[key] = entry
        return value

    for number, line in enumerate(lines, 1):
        if not line.strip():
            continue
        try:
            run = json.loads(line, object_pairs_hook=unique_fields)
            validate_tests(run)
        except (ValueError, TypeError) as error:
            raise ValueError(f'test run line {number}: {error}') from error
        if folded is None:
            folded = dict(run, failed=[], flaky=[], failed_count=0)
        own = set(run['failed'])
        summed += run['failed_count'] - len(earlier & own)
        if summed > 2 ** 32 - 1:
            raise ValueError(f'test run line {number}: failed count overflow')
        capped |= run['failed_count'] >= 100
        for name in run['failed']:
            if name not in earlier:
                folded['failed'].append(name)
                earlier.add(name)
        for flaky in run['flaky']:
            if flaky['name'] not in flaky_names:
                folded['flaky'].append(flaky)
                flaky_names.add(flaky['name'])
        if precedence.index(run['rerun']) > precedence.index(folded['rerun']):
            folded['rerun'] = run['rerun']
    if folded is not None:
        folded['failed_count'] = summed if capped else len(earlier)
        folded['failed'] = folded['failed'][:100]
        folded['flaky'] = folded['flaky'][:25]
        validate_tests(folded)
        with Path(os.environ['AF_CHECK_REPORT']).open('a') as stream:
            stream.write(json.dumps(folded, sort_keys=True) + '\n')


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
    if len(sys.argv) == 3 and sys.argv[1] == '--finish-tests':
        try:
            finish_tests(sys.argv[2])
        except (OSError, ValueError, KeyError) as error:
            print(f'ci-step.py: cannot finish test report: {error}', file=sys.stderr)
            return 1
        return 0
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
        metrics = Path(path)
        metrics.parent.mkdir(parents=True, exist_ok=True)
        with metrics.open('a') as stream:
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
