#!/usr/bin/env python3
"""Capture exact merged release PR labels once; retries read only the tag annotation."""
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('classes', Path(__file__).with_name('test-classes.py'))
tc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tc)


def git(*args):
    return subprocess.check_output(['git', *args], text=True).strip()


def capture(sha, version):
    repository = os.environ.get('GH_REPO') or os.environ.get('GITHUB_REPOSITORY', '')
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository):
        raise ValueError('release label capture requires GH_REPO=OWNER/REPO')
    if not re.fullmatch(r'[a-f0-9]{40}', sha):
        raise ValueError('release label capture requires an exact commit SHA')
    matches = []
    # Page explicitly rather than trusting the first page or a latest-release query.
    for page in range(1, 101):
        endpoint = f'repos/{repository}/commits/{sha}/pulls?per_page=100&page={page}'
        raw = subprocess.check_output(['gh', 'api', '--method', 'GET', endpoint],
                                      text=True, timeout=30)
        rows = json.loads(raw)
        if not isinstance(rows, list) or len(rows) > 100:
            raise ValueError('invalid commit PR API page')
        for pr in rows:
            if not isinstance(pr, dict):
                raise ValueError('invalid commit PR API entry')
            if (pr.get('merge_commit_sha') == sha and pr.get('merged_at')
                    and pr.get('base', {}).get('ref') == 'main'
                    and pr.get('head', {}).get('ref') == 'release/v' + version):
                source = {'repository': repository, 'number': pr['number'],
                          'merge_commit_sha': sha, 'base': 'main',
                          'head': 'release/v' + version, 'merged_at': pr['merged_at']}
                tc.validate_source_pr(source, sha, version)
                labels = tc.event_labels({'pull_request': pr})
                # Validate even when the eventual match count proves ambiguous.
                tc.resolve(labels, 'release', sha, version)
                matches.append((source, labels))
        if len(rows) < 100:
            break
    else:
        raise ValueError('commit PR API pagination limit exceeded')
    if len(matches) > 1:
        raise ValueError('multiple exact merged release PRs; refusing ambiguous labels')
    return matches[0] if matches else (None, [])


def prepare(sha, version, output):
    if git('rev-parse', 'HEAD') != sha:
        raise ValueError('release policy checkout SHA mismatch')
    source, labels = capture(sha, version)
    binding = {'schema': tc.BINDING, 'sha': sha, 'version': version,
               'source_pr': source, 'plan': tc.resolve(labels, 'release', sha, version)}
    tc.release_plan(json.dumps(binding), sha, version)
    tc.write(output, binding)


def resolve(sha, version, tag, output):
    annotation = ''
    if tag:
        ref = f'refs/tags/{tag}'
        result = subprocess.run(['git', 'show-ref', '--verify', ref], capture_output=True)
        if result.returncode == 0:
            if git('rev-parse', ref + '^{}') != sha:
                raise ValueError('release tag does not name the triggering commit')
            if git('cat-file', '-t', ref) == 'tag':
                annotation = git('for-each-ref', '--format=%(contents)', ref)
    plan = tc.release_plan(annotation, sha, version)
    tc.write(output, plan)
    with Path(os.environ['GITHUB_OUTPUT']).open('a') as stream:
        stream.write('test_plan=' + json.dumps(plan, separators=(',', ':')) + '\n')
    if os.getenv('GITHUB_STEP_SUMMARY'):
        with Path(os.environ['GITHUB_STEP_SUMMARY']).open('a') as stream:
            stream.write(f"Release validation: **{plan['profile']}**; SHA `{sha}`; version `{version}`.\n\n")
            for kind, row in plan['classes'].items():
                stream.write(f"- {kind}: {row['state']} ({row['reason']})\n")
            stream.write('\nPackaged binary, architecture, archive and signature smoke remain mandatory. '
                         'Published-release performance measurement is asynchronous and does not gate publication.\n')


if __name__ == '__main__':
    try:
        if sys.argv[1] == 'prepare':
            prepare(*sys.argv[2:])
        elif sys.argv[1] == 'resolve':
            resolve(*sys.argv[2:])
        else:
            raise ValueError('expected prepare or resolve')
    except (ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError) as error:
        sys.exit(str(error))
