#!/usr/bin/env python3
"""Compare complete workspace fixture suites at two Git commits on this runner (stdlib only)."""
import argparse
import contextlib
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
import platform
import signal
import statistics
import subprocess
import sys
import tarfile
import tempfile
import time

sys.dont_write_bytecode = True
KIND = 'af/workspace-fixture-benchmark@1'
SUITE = 'scripts/test-scenario-git-workspace.py'
# Passed as code to the selected Python, never imported from the older source tree.
RECORDER = r'''
import json, pathlib, runpy, sys, unittest
sys.dont_write_bytecode = True
script, receipt = sys.argv[1:]
sys.argv = [script]
original = unittest.TextTestRunner
class Runner(original):
    def run(self, suite):
        expected = suite.countTestCases()
        result = super().run(suite)
        pathlib.Path(receipt).write_text(json.dumps(dict(
            schema='af/workspace-fixture-suite@1', expected=expected, tests=result.testsRun, successful=result.wasSuccessful(),
            failures=len(result.failures), errors=len(result.errors),
            skipped=len(result.skipped), expected_failures=len(result.expectedFailures),
            unexpected_successes=len(result.unexpectedSuccesses))))
        return result
unittest.TextTestRunner = Runner
unittest.runner.TextTestRunner = Runner
runpy.run_path(script, run_name='__main__')
'''


def digest(data):
    return hashlib.sha256(data).hexdigest()


def git(repo, *args):
    return subprocess.check_output(['git', '-C', str(repo), *args])


def resolve(repo, ref):
    return git(repo, 'rev-parse', '--verify', '--end-of-options', ref + '^{commit}').decode().strip()


def export(repo, commit, root):
    """Export only pinned Git objects. Refuse unsafe tar paths without relying on new Python."""
    root.mkdir()
    with tempfile.TemporaryFile() as archive:
        subprocess.run(['git', '-C', str(repo), 'archive', '--format=tar', commit],
                       stdout=archive, check=True)
        archive.seek(0)
        with tarfile.open(fileobj=archive) as tar:
            for member in tar:
                path = PurePosixPath(member.name)
                if path.is_absolute() or '..' in path.parts:
                    raise ValueError('unsafe source archive path')
                dest = root.joinpath(*path.parts)
                if any((root / parent).is_symlink() for parent in path.parents):
                    raise ValueError('source archive traverses a symlink')
                if member.isdir():
                    dest.mkdir(exist_ok=True)
                elif member.isfile():
                    dest.parent.mkdir(parents=True, exist_ok=True)
                    with tar.extractfile(member) as source, dest.open('xb') as output:
                        output.write(source.read())
                    dest.chmod(member.mode & 0o777)
                elif member.issym():
                    target = PurePosixPath(member.linkname)
                    if target.is_absolute() or not (dest.parent / member.linkname).resolve().is_relative_to(root):
                        raise ValueError('source archive symlink escapes tree')
                    dest.symlink_to(member.linkname)
                else:
                    raise ValueError('unsupported source archive entry')


def tree_digest(root):
    entries = []
    for path in sorted(root.rglob('*')):
        if path.is_symlink():
            value = ['link', os.readlink(path)]
        elif path.is_file():
            value = ['file', path.stat().st_mode & 0o777, digest(path.read_bytes())]
        else:
            value = ['directory']
        entries.append([path.relative_to(root).as_posix(), value])
    return digest(json.dumps(entries, separators=(',', ':')).encode())


def run_suite(root, output, label, pair, timeout):
    log = output / f'{pair}-{label}.log'
    receipt = output / f'{pair}-{label}.receipt.json'
    command = [str(Path(sys.executable).resolve()), '-I', '-S', '-B', '-c', RECORDER,
               str(root / SUITE), str(receipt)]
    started = time.perf_counter()
    timed_out = False
    with log.open('wb') as stream:
        # Own one fresh process group so a timed-out suite cannot leave Git children
        # contaminating later samples. Signal only the group created by this invocation.
        with subprocess.Popen(command, cwd=root, stdout=stream, stderr=subprocess.STDOUT,
                              start_new_session=True) as process:
            try:
                code = process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass  # The group exited between the deadline and the signal.
                process.wait()
                code, timed_out = None, True
    elapsed = time.perf_counter() - started
    try:
        counts = json.loads(receipt.read_text())
        check_shape(counts, suite_schema())
    except (OSError, ValueError):
        counts = None
    return dict(pair=pair, side=label, wall_seconds=elapsed, exit_code=code,
                timed_out=timed_out, counts=counts, log=log.name, receipt=receipt.name)


def evaluate(runs, pairs):
    reasons = []
    expected_order = [(pair, side) for pair in range(1, pairs + 1)
                      for side in (('base', 'head') if pair % 2 else ('head', 'base'))]
    if [(r['pair'], r['side']) for r in runs] != expected_order:
        reasons.append('incomplete or non-alternating paired runs')
    totals = set()
    for r in runs:
        c = r['counts']
        if r['exit_code'] != 0 or r['timed_out']:
            reasons.append(f"{r['pair']}-{r['side']}: suite failed or timed out")
        if (not isinstance(c, dict) or set(c) != {'schema', 'expected', 'tests', 'successful', 'failures', 'errors',
                                                'skipped', 'expected_failures', 'unexpected_successes'}
                or c['schema'] != 'af/workspace-fixture-suite@1'
                or any(type(c[k]) is not int or c[k] < 0 for k in c if k not in ('schema', 'successful'))
                or c['tests'] <= 0 or c['tests'] != c['expected'] or c['successful'] is not True
                or any(c[k] != 0 for k in ('failures', 'errors', 'skipped', 'expected_failures', 'unexpected_successes'))):
            reasons.append(f"{r['pair']}-{r['side']}: missing, incomplete or failing unittest receipt")
        else:
            totals.add(c['tests'])
        if not math.isfinite(r['wall_seconds']) or r['wall_seconds'] <= 0:
            reasons.append('invalid wall time')
    if len(totals) != 1:
        reasons.append('unequal or absent test counts')
    comparison = None
    if not reasons:
        base = statistics.median(r['wall_seconds'] for r in runs if r['side'] == 'base')
        head = statistics.median(r['wall_seconds'] for r in runs if r['side'] == 'head')
        deltas = [next(r['wall_seconds'] for r in runs if r['pair'] == p and r['side'] == 'head') -
                  next(r['wall_seconds'] for r in runs if r['pair'] == p and r['side'] == 'base')
                  for p in range(1, pairs + 1)]
        comparison = dict(base_median_seconds=base, head_median_seconds=head,
                          head_minus_base_seconds=head - base, head_minus_base_percent=100 * (head - base) / base,
                          paired_head_minus_base_median_seconds=statistics.median(deltas))
    return reasons, comparison


def suite_schema():
    properties = dict(schema=dict(const='af/workspace-fixture-suite@1'))
    properties.update({k: dict(type='boolean') if k == 'successful' else dict(type='integer', minimum=0)
                       for k in ('expected', 'tests', 'successful', 'failures', 'errors', 'skipped',
                                 'expected_failures', 'unexpected_successes')})
    return dict(type='object', additionalProperties=False, required=list(properties), properties=properties)


@contextlib.contextmanager
def frozen(roots):
    """Keep source exports read-only for every run, then restore directory cleanup access."""
    paths = [p for root in roots for p in [root, *root.rglob('*')] if not p.is_symlink()]
    modes = [(path, path.stat().st_mode & 0o777) for path in paths]
    try:
        for path, mode in modes:
            path.chmod(mode & ~0o222)
        yield
    finally:
        for path, mode in modes:
            path.chmod(mode)


def report_schema():
    """The writer/reader's versioned evidence shape; semantic checks live in evaluate/read_report."""
    def obj(properties):
        return dict(type='object', additionalProperties=False, required=list(properties), properties=properties)
    string = dict(type='string')
    number = dict(type='number', minimum=0)
    identity = obj(dict(commit=string, git_tree=string, export_sha256=string, suite_sha256=string))
    counts = suite_schema()
    comparison = obj({k: dict(type='number') for k in ('base_median_seconds', 'head_median_seconds',
        'head_minus_base_seconds', 'head_minus_base_percent', 'paired_head_minus_base_median_seconds')})
    schema = obj(dict(schema=dict(const=KIND), suite=dict(const=SUITE), pairs=dict(type='integer', minimum=3),
        platform=string, interpreter=obj(dict(path=string, version=string, sha256=string)),
        sources=obj(dict(base=identity, head=identity)),
        runs=dict(type='array', items=obj(dict(pair=dict(type='integer', minimum=1),
            side=dict(enum=['base', 'head']), wall_seconds=number,
            exit_code=dict(type=['integer', 'null']), timed_out=dict(type='boolean'),
            counts=dict(anyOf=[counts, dict(type='null')]), log=string, receipt=string))),
        valid=dict(type='boolean'), errors=dict(type='array', items=string),
        comparison=dict(anyOf=[comparison, dict(type='null')])))
    return dict(schema, **{'$schema': 'https://json-schema.org/draft/2020-12/schema',
                          '$id': 'urn:af:schema:workspace-fixture-benchmark:1'})


def check_shape(value, schema):
    """Validate the small stdlib evidence contract, using exactly the shipped schema shape."""
    if 'anyOf' in schema:
        for branch in schema['anyOf']:
            try:
                check_shape(value, branch)
                return
            except ValueError:
                pass
        raise ValueError('evidence matches no schema alternative')
    kinds = schema.get('type', [])
    kinds = [kinds] if isinstance(kinds, str) else kinds
    matches = dict(object=isinstance(value, dict), array=isinstance(value, list),
                   string=isinstance(value, str), boolean=type(value) is bool,
                   integer=type(value) is int, number=type(value) in (int, float), null=value is None)
    if kinds and not any(matches[k] for k in kinds):
        raise ValueError('evidence has an incorrect type')
    if (('const' in schema and value != schema['const'])
            or ('enum' in schema and value not in schema['enum'])):
        raise ValueError('unsupported evidence discriminator')
    if type(value) in (int, float):
        if not math.isfinite(value) or value < schema.get('minimum', -math.inf):
            raise ValueError('evidence has an invalid number')
    if isinstance(value, dict):
        if set(value) != set(schema['properties']):
            raise ValueError('evidence fields differ from schema')
        for key, child in schema['properties'].items():
            check_shape(value[key], child)
    if isinstance(value, list):
        for item in value:
            check_shape(item, schema['items'])


def read_report(path):
    report = json.loads(Path(path).read_text())
    check_shape(report, report_schema())
    if type(report['pairs']) is not int or report['pairs'] < 3:
        raise ValueError('benchmark requires at least three pairs')
    reasons, comparison = evaluate(report['runs'], report['pairs'])
    # Additional errors (e.g. changed exports) must never be converted to valid evidence.
    if report['errors'][:len(reasons)] != reasons or report['valid'] != (not report['errors']):
        raise ValueError('benchmark validity differs from recorded evidence')
    if report['comparison'] != (comparison if report['valid'] else None):
        raise ValueError('benchmark comparison differs from recorded evidence')
    return report


def summary(report):
    lines = ['## Workspace fixture paired benchmark', '',
             'Evidence: ' + ('valid' if report['valid'] else 'INVALID'), '',
             f"Python: `{report['interpreter']['path']}` ({report['interpreter']['version'].replace(chr(10), ' ')})", '',
             f"Platform: {report['platform']}", '',
             f"Base: `{report['sources']['base']['commit']}`; head: `{report['sources']['head']['commit']}`", '',
             '| Pair | Side | Seconds | Exit | Tests |', '| --- | --- | ---: | ---: | ---: |']
    for r in report['runs']:
        count = (r['counts'] or {}).get('tests', 'missing')
        lines.append(f"| {r['pair']} | {r['side']} | {r['wall_seconds']:.3f} | {r['exit_code']} | {count} |")
    if report['valid']:
        c = report['comparison']
        lines += ['', f"Base median {c['base_median_seconds']:.3f}s; head median {c['head_median_seconds']:.3f}s.",
                  f"Head minus base: {c['head_minus_base_seconds']:+.3f}s ({c['head_minus_base_percent']:+.2f}%).",
                  f"Median paired head minus base: {c['paired_head_minus_base_median_seconds']:+.3f}s.",
                  'Observed timings only; no performance threshold is asserted.']
    else:
        lines += ['', *report['errors']]
    return '\n'.join(lines) + '\n'


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, default=Path.cwd())
    parser.add_argument('--base', required=True)
    parser.add_argument('--head', required=True)
    parser.add_argument('--output', type=Path, required=True, help='new directory for JSON, summary and per-run logs')
    parser.add_argument('--pairs', type=int, default=3)
    parser.add_argument('--timeout', type=int, default=600, help='seconds per complete suite process')
    parser.add_argument('--summary', type=Path, help='append the report to a GitHub job summary')
    args = parser.parse_args(argv)
    if args.pairs < 3 or args.timeout <= 0:
        parser.error('at least three pairs and a positive suite timeout are required')
    repo, output = args.repo.resolve(), args.output.resolve()
    commits = {side: resolve(repo, ref) for side, ref in [('base', args.base), ('head', args.head)]}
    output.mkdir(parents=True, exist_ok=False)
    python = Path(sys.executable).resolve(strict=True)
    report = dict(schema=KIND, suite=SUITE, pairs=args.pairs, platform=platform.platform(),
                  interpreter=dict(path=str(python), version=sys.version, sha256=digest(python.read_bytes())),
                  sources={}, runs=[], valid=False, errors=[], comparison=None)
    with tempfile.TemporaryDirectory(prefix='workspace-benchmark-') as tmp:
        roots = {side: Path(tmp).resolve() / side for side in commits}
        for side, root in roots.items():
            export(repo, commits[side], root)
        with frozen(roots.values()):
            for side, root in roots.items():
                report['sources'][side] = dict(commit=commits[side],
                    git_tree=git(repo, 'rev-parse', commits[side] + '^{tree}').decode().strip(),
                    export_sha256=tree_digest(root), suite_sha256=digest((root / SUITE).read_bytes()))
            for pair in range(1, args.pairs + 1):
                for side in (('base', 'head') if pair % 2 else ('head', 'base')):
                    report['runs'].append(run_suite(roots[side], output, side, pair, args.timeout))
            report['errors'], comparison = evaluate(report['runs'], args.pairs)
            for side, root in roots.items():
                if tree_digest(root) != report['sources'][side]['export_sha256']:
                    report['errors'].append(side + ' source export changed during benchmark')
            report['valid'] = not report['errors']
            report['comparison'] = comparison if report['valid'] else None
    path = output / 'benchmark.json'
    path.write_text(json.dumps(report, indent=2, allow_nan=False) + '\n')
    text = summary(read_report(path))
    (output / 'summary.md').write_text(text)
    if args.summary:
        with args.summary.open('a') as stream:
            stream.write(text)
    print(text)
    return 0 if report['valid'] else 1


if __name__ == '__main__':
    sys.exit(main())
