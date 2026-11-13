#!/usr/bin/env python3
"""Reconcile one protected runner's draft/assets before publication; never clobber."""
import argparse
import importlib.util
import hashlib
import json
import os
import re
from pathlib import Path
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True

_observation_spec = importlib.util.spec_from_file_location(
    'release_observation', Path(__file__).resolve().with_name('release-observation.py'))
observation = importlib.util.module_from_spec(_observation_spec)
_observation_spec.loader.exec_module(observation)


def gh(*args):
    result = subprocess.run(['gh', *map(str, args)], capture_output=True, text=True)
    if result.returncode:
        raise ValueError('GitHub effect/read failed; rerun to reconcile before retry')
    return result.stdout


def publish(tag, source, files, notes):
    if not re.fullmatch(r'v[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[1-9][0-9]*)?', tag) or not re.fullmatch('[0-9a-f]{40}', source):
        raise ValueError('expected exact release tag and source SHA')
    provenance = json.loads(Path('dist/release-provenance.json').read_text())
    if (set(provenance) != {'schema', 'version', 'source', 'repository', 'run_id'}
            or provenance['schema'] != 'af.release-provenance/1'
            or not re.fullmatch('[0-9]+', provenance['run_id'])
            or provenance['source'] != source or provenance['version'] != tag[1:]
            or provenance['repository'] != os.environ['GH_REPO']):
        raise ValueError('local provenance differs from publication identity')
    targets = json.loads(Path('scripts/release-policy.json').read_text())['targets']
    expected_archives = {'af-' + tag + '-' + target + '.tar.gz' for target in targets}
    if {p.name for p in files if p.suffix == '.gz'} != expected_archives:
        raise ValueError('platform archive inventory differs from reviewed policy')
    subprocess.run(['minisign', '-V', '-q', '-m', 'SHA256SUMS',
                    '-p', 'crates/af/keys/release.pub'], check=True)
    sums = dict((name, checksum) for checksum, name in
                (line.split() for line in Path('SHA256SUMS').read_text().splitlines()))
    if set(sums) != expected_archives | {'release-provenance.json'}:
        raise ValueError('signed checksum inventory differs from reviewed policy')
    for path in files:
        if path.name in expected_archives | {'release-provenance.json'}:
            if hashlib.sha256(path.read_bytes()).hexdigest() != sums.get(path.name):
                raise ValueError('local signed digest differs: ' + path.name)
    rows = observation.releases(lambda endpoint: gh(
        'api', 'repos/{owner}/{repo}/' + endpoint, '--include'))
    release = next((row for row in rows if row['tag_name'] == tag), None)
    if release is None:
        flags = ['--prerelease'] if '-rc.' in tag else []
        gh('release', 'create', tag, '--verify-tag', '--target', source,
           '--title', tag, '--notes-file', notes, '--draft', *flags)
        # A lost reply returns failure; next invocation reads the durable draft.
        rows = observation.releases(lambda endpoint: gh(
            'api', 'repos/{owner}/{repo}/' + endpoint, '--include'))
        release = next(row for row in rows if row['tag_name'] == tag)
    if release['target_commitish'] != source:
        raise ValueError('release target differs from exact publication source')
    existing = {asset['name'] for asset in release['assets']}
    expected = {path.name for path in files}
    if existing - expected:
        raise ValueError('foreign release assets; operator reconciliation required')
    for path in files:
        if path.name in existing:
            with tempfile.TemporaryDirectory() as temporary:
                gh('release', 'download', tag, '--pattern', path.name, '--dir', temporary)
                downloaded = Path(temporary) / path.name
                if downloaded.read_bytes() != path.read_bytes():
                    raise ValueError('conflicting published asset ' + path.name)
        else:
            if not release['draft']:
                raise ValueError('published release is incomplete; refuse mutation')
            gh('release', 'upload', tag, path)
    # Verify the entire remote inventory before the visibility effect.
    remote = json.loads(gh('api', 'repos/{owner}/{repo}/releases/' + str(release['id'])))
    if {a['name'] for a in remote['assets']} != expected:
        raise ValueError('remote assets incomplete; rerun to reconcile')
    if release['draft']:
        gh('release', 'edit', tag, '--draft=false')
    print('published %s; assets %s' % (tag, hashlib.sha256('\n'.join(sorted(expected)).encode()).hexdigest()))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('tag')
    parser.add_argument('source')
    args = parser.parse_args()
    try:
        publish(args.tag, args.source,
                [*sorted(Path('dist').glob('*.tar.gz')), Path('dist/release-provenance.json'), Path('SHA256SUMS'),
                 Path('SHA256SUMS.minisig'), Path('install.sh')], Path('notes.md'))
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print('release-publish: ' + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
