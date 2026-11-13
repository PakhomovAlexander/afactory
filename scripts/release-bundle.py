#!/usr/bin/env python3
"""Restore the original signed asset bundle before any publication retry."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def restore(tag, source):
    name = 'release-signed-' + tag + '-' + source
    pages = json.loads(subprocess.check_output([
        'gh', 'api', 'repos/{owner}/{repo}/actions/artifacts?name=' + name + '&per_page=100',
        '--paginate', '--slurp'], text=True))
    bundles = [a for page in pages for a in page['artifacts'] if a['name'] == name]
    if not bundles:
        # Refuse rebuilding after partial publication when its original bundle was
        # deleted or expired. Never adopt unverified assets from a draft.
        releases = json.loads(subprocess.check_output([
            'gh', 'api', 'repos/{owner}/{repo}/releases?per_page=100'], text=True))
        if any(r['tag_name'] == tag and r['assets'] for r in releases):
            raise ValueError('original signed bundle unavailable; restore retained artifact before retry')
        return False
    if len(bundles) != 1 or bundles[0]['expired']:
        raise ValueError('original signed bundle ambiguous or expired; operator recovery required')
    artifact = bundles[0]
    origin = artifact['workflow_run']
    run = json.loads(subprocess.check_output([
        'gh', 'api', 'repos/{owner}/{repo}/actions/runs/' + str(origin['id'])], text=True))
    workflow = json.loads(subprocess.check_output([
        'gh', 'api', 'repos/{owner}/{repo}/actions/workflows/release.yml'], text=True))
    canonical, separator, ref = run['path'].partition('@')
    if (run.get('workflow_id') != workflow['id'] or (separator and not ref.strip())
            or origin['head_sha'] != source or run['head_sha'] != source
            or canonical != '.github/workflows/release.yml' or run['event'] != 'push'):
        raise ValueError('signed bundle has foreign workflow/source provenance')
    with tempfile.TemporaryDirectory() as temporary:
        subprocess.run(['gh', 'run', 'download', str(origin['id']), '--name', name,
                        '--dir', temporary], check=True)
        root = Path(temporary)
        expected = {'dist', 'SHA256SUMS', 'SHA256SUMS.minisig', 'install.sh'}
        if {p.name for p in root.iterdir()} != expected:
            raise ValueError('signed bundle inventory differs')
        provenance = json.loads((root / 'dist/release-provenance.json').read_text())
        if (provenance['source'] != source or provenance['version'] != tag[1:]
                or provenance['repository'] != os.environ['GH_REPO']
                or str(provenance['run_id']) != str(origin['id'])):
            raise ValueError('signed bundle provenance differs')
        subprocess.run(['minisign', '-V', '-q', '-m', str(root / 'SHA256SUMS'),
                        '-p', 'crates/af/keys/release.pub'], check=True)
        shutil.rmtree('dist')
        shutil.copytree(root / 'dist', 'dist')
        for filename in expected - {'dist'}:
            shutil.copyfile(root / filename, filename)
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('tag')
    parser.add_argument('source')
    args = parser.parse_args()
    reused = restore(args.tag, args.source)
    with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
        output.write('reused=' + str(reused).lower() + '\n')


if __name__ == '__main__':
    main()
