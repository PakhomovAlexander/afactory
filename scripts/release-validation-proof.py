#!/usr/bin/env python3
"""Plan hosted validation and simulate release binding locally, without publication."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
import unittest

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts' / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


tc = load('proof_classes', 'test-classes.py')


def proof_plan(sha, version, event_name, profile='', event=None):
    if not re.fullmatch(r'[a-f0-9]{40}', sha):
        raise ValueError('proof requires an exact checkout SHA')
    if not version:
        raise ValueError('proof requires a workspace version')
    if event_name == 'pull_request':
        labels = tc.event_labels(event)
        profiles = set(labels) & {'release:fast', 'release:full'}
        if len(profiles) != 1:
            raise ValueError('proof PR requires exactly one release:fast/full label')
        # Other test/no-* labels select ordinary PR validation, never this explicit proof.
        profile = next(iter(profiles)).split(':')[1]
    elif event_name != 'workflow_dispatch':
        raise ValueError('proof supports only pull_request and workflow_dispatch')
    if profile not in ('fast', 'full'):
        raise ValueError('proof requires fast or full')
    return tc.validate_plan(tc.resolve(['release:' + profile], 'release', sha, version))


def simulate(output):
    """Run the existing real shell resolver fixtures with only local disposable remotes."""
    fixtures = load('proof_resolver_fixtures', 'test-release-resolve.py')
    suite = unittest.defaultTestLoader.loadTestsFromModule(fixtures)
    scratch = output / 'scratch'
    scratch.mkdir(parents=True, exist_ok=True)
    previous = tempfile.tempdir
    previous_protocols = os.environ.get('GIT_ALLOW_PROTOCOL')
    # Even an accidental new fixture remote cannot contact a network transport.
    os.environ['GIT_ALLOW_PROTOCOL'] = 'file'
    tempfile.tempdir = str(scratch.resolve())
    started = time.monotonic()
    try:
        with (output / 'local-binding.log').open('w') as stream:
            result = unittest.TextTestRunner(stream=stream, verbosity=2).run(suite)
    finally:
        tempfile.tempdir = previous
        if previous_protocols is None:
            os.environ.pop('GIT_ALLOW_PROTOCOL', None)
        else:
            os.environ['GIT_ALLOW_PROTOCOL'] = previous_protocols
    evidence = {'kind': 'local release binding simulation', 'publication_executed': False,
                'tests_run': result.testsRun, 'failures': len(result.failures),
                'errors': len(result.errors), 'skipped': len(result.skipped),
                'elapsed_seconds': time.monotonic() - started,
                'successful': result.wasSuccessful() and not result.skipped and result.testsRun > 0}
    tc.write(output / 'local-binding-results.json', evidence)
    if not evidence['successful']:
        raise ValueError('local release binding simulation failed; see retained log/results')
    return evidence


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', default='')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    sha = os.environ['GITHUB_SHA']
    head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
    if head != sha:
        raise ValueError('proof checkout differs from github.sha')
    version = re.search(r'^version = "([^"]+)"$', (ROOT / 'Cargo.toml').read_text(), re.M)
    if version is None:
        raise ValueError('Cargo.toml has no workspace version')
    event_name = os.environ['GITHUB_EVENT_NAME']
    event = tc.read(os.environ['GITHUB_EVENT_PATH']) if event_name == 'pull_request' else None
    tc.audit_source()
    plan = proof_plan(sha, version.group(1), event_name, args.profile, event)
    tc.write(args.output / 'plan.json', plan)
    evidence = simulate(args.output)
    evidence.update(sha=sha, version=plan['version'], catalog_sha256=plan['catalog_sha256'],
                    profile=plan['profile'])
    tc.write(args.output / 'local-binding-results.json', evidence)
    with Path(os.environ['GITHUB_OUTPUT']).open('a') as stream:
        stream.write('test_plan=' + json.dumps(plan, separators=(',', ':')) + '\n')
    summary = (f"Release validation proof: **{plan['profile']}**; SHA `{sha}`; "
               f"workspace version `{plan['version']}`.\n\n"
               f"Local binding simulation: {evidence['tests_run']} tests passed. "
               "No release publication executed or timed; no savings claim.\n\n")
    summary += '\n'.join(f"- {kind}: {row['state']} ({row['reason']})"
                         for kind, row in plan['classes'].items()) + '\n'
    (args.output / 'summary.md').write_text(summary)
    if os.getenv('GITHUB_STEP_SUMMARY'):
        with Path(os.environ['GITHUB_STEP_SUMMARY']).open('a') as stream:
            stream.write(summary)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        sys.exit(str(error))
