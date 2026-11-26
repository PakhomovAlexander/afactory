#!/usr/bin/env python3
"""Compare exact source trees using the existing release CLI's disposable real-Git fixture."""
import argparse
import difflib
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shlex
import statistics
import subprocess
import sys
import time
from unittest import mock

sys.dont_write_bytecode = True


def load(root, name):
    spec = importlib.util.spec_from_file_location(name, root / 'scripts' / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def fingerprint(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()


def measure(trace, action):
    trace.unlink(missing_ok=True)
    start = time.perf_counter()
    result = action()
    elapsed = time.perf_counter() - start
    starts = [row for line in trace.read_text().splitlines()
              if (row := json.loads(line))['event'] == 'start']
    return result, {'seconds': round(elapsed, 6), 'git_processes': len(starts)}


def run_case(root, refusals):
    tests = load(root, 'test-dev-release')
    controller = load(root, 'dev-release')
    fixture = tests.ReleaseCLI('test_plan_uses_canonical_repository_and_preserves_nested_queries')
    try:
        fixture.setUp()
        trace = fixture.base / 'git-trace.jsonl'
        shim = fixture.bin / 'git'
        shim.write_text('#!/bin/sh\nexport GIT_TRACE2_EVENT=' + shlex.quote(str(trace))
                        + '\nexec ' + shlex.quote(tests.GIT) + ' "$@"\n')
        result, plan_stats = measure(trace, lambda: fixture.cli('plan'))
        state = json.loads(result.stdout)
        plan = state['plan']
        source = plan['source']
        assert source == fixture.initial and state['authorization'] is None
        assert not fixture.data().get('effects') and not (fixture.state / 'candidate').exists()
        receipt = state['receipts']['preflight']
        features = receipt['features']
        paths = json.loads((root / 'scripts/release-policy.json').read_text())['required_features']
        assert len(features) == len(paths) == 54
        assert plan['executor'] == controller.digest((root / 'scripts/dev-release.py').read_text())
        executor = controller.Controller(None, fixture.state)

        def read_features():
            if hasattr(executor, 'batch_blobs'):
                blobs = executor.batch_blobs(source, paths)
                assert len(blobs) == 54
                return {path: executor.blob(source, path, blobs) for path in paths}
            return {path: executor.blob(source, path) for path in paths}

        with mock.patch.dict(os.environ, fixture.env, clear=True):
            contents, read_stats = measure(trace, read_features)
        oracle = {path: fingerprint(fixture.git('--git-dir', executor.mirror, 'show',
                                                source + ':' + path)) for path in paths}
        assert features == oracle == {path: fingerprint(content) for path, content in contents.items()}
        # Source and executor are checked above. These vary with exact source bytes.
        # Paths and timestamps also vary with each newly allocated fixture.
        projection = {
            'plan': {key: val for key, val in plan.items()
                     if key not in ('source', 'remote', 'executor', 'date')},
            'receipt': {key: val for key, val in receipt.items() if key not in ('source', 'at')},
            'schema': state['schema'], 'status': state['status'], 'pin': state['pin'],
        }
        report = {'plan': {**plan_stats, 'exit': result.returncode},
                  'isolated_54': {**read_stats, 'exit': 0},
                  'feature_receipt_sha256': fingerprint(features),
                  'plan_projection_sha256': fingerprint(projection)}
        if refusals:
            (fixture.state / 'run.json').unlink()
            feature = next(iter(paths))
            first = fixture.source / feature
            outcomes = []
            for label, mutate, diagnostic in (
                ('malformed', lambda: first.write_text('{malformed'),
                 'source_inclusion: invalid required schema ' + feature),
                ('missing', first.unlink, 'command_failed: git -c (exit 128)'),
            ):
                mutate()
                fixture.commit(label)
                fixture.git('-C', fixture.source, 'push', str(fixture.remote), 'main')
                result, stats = measure(trace, lambda: fixture.cli('plan', ok=False))
                assert 'authoritative main/tag fetch complete' in result.stderr
                assert fixture.git('--git-dir', executor.mirror, 'rev-parse', 'refs/heads/main') == (
                    fixture.git('-C', fixture.source, 'rev-parse', 'HEAD'))
                assert diagnostic in result.stderr, result.stderr
                assert not (fixture.state / 'run.json').exists() and not fixture.data().get('effects')
                outcomes.append({'case': label, **stats, 'exit': result.returncode,
                                 'diagnostic': diagnostic})
            report['refusals'] = outcomes
        return report
    finally:
        fixture.doCleanups()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('parent', type=lambda path: Path(path).resolve())
    parser.add_argument('candidate', type=lambda path: Path(path).resolve())
    args = parser.parse_args()
    roots = {'parent': args.parent, 'candidate': args.candidate}
    filename = b'scripts/dev-release.py'
    patch = difflib.diff_bytes(difflib.unified_diff, *(
        (root / os.fsdecode(filename)).read_bytes().splitlines(keepends=True)
        for root in roots.values()), fromfile=b'a/' + filename, tofile=b'b/' + filename)
    print(json.dumps({'implementation_patch_sha256': hashlib.sha256(b''.join(patch)).hexdigest(),
                      'git': subprocess.check_output(['git', '--version'], text=True).strip(),
                      'python': sys.version.split()[0]}), flush=True)
    runs = {'parent': [], 'candidate': []}
    for repeat, order in enumerate((('parent', 'candidate'), ('candidate', 'parent'),
                                    ('parent', 'candidate')), start=1):
        for side in order:
            report = run_case(roots[side], refusals=repeat == 1)
            runs[side].append(report)
            print(json.dumps({'pair': repeat, 'side': side, **report}), flush=True)
    for key in ('feature_receipt_sha256', 'plan_projection_sha256'):
        assert len({row[key] for samples in runs.values() for row in samples}) == 1
    for side, rows in runs.items():
        print(json.dumps({'side': side, 'median_seconds': {
            phase: statistics.median(row[phase]['seconds'] for row in rows)
            for phase in ('plan', 'isolated_54')}}))


if __name__ == '__main__':
    main()
