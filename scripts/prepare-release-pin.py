#!/usr/bin/env python3
"""Prepare only the repository af pin from an explicitly selected signed published release."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import selectors
import stat
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts/performance'))
from perflab._vendor import tomli
spec = importlib.util.spec_from_file_location('af_pin', ROOT / 'scripts/af-pin.py')
pin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pin)


def command(argv, limit=128 * 1024):
    """Bound transport time/output and discard potentially credential-bearing diagnostics."""
    with subprocess.Popen(argv, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                          env={**os.environ, 'GH_HOST': 'github.com'}) as process:
        try:
            data = bytearray()
            deadline = time.monotonic() + 60
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                while True:
                    seconds = deadline - time.monotonic()
                    if seconds <= 0 or not selector.select(seconds):
                        raise ValueError('release command timed out')
                    chunk = os.read(process.stdout.fileno(), min(8192, limit + 1 - len(data)))
                    if not chunk:
                        break
                    data.extend(chunk)
                    if len(data) > limit:
                        raise ValueError('release response exceeds byte budget')
            if process.wait(timeout=max(.01, deadline - time.monotonic())):
                raise ValueError('release transport/signature command failed')
            return bytes(data)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()


def owned(path, directory=False):
    for component in (*reversed(path.parents), path):
        if component.is_symlink():
            raise ValueError('symlink path refused')
    metadata = path.stat()
    if (metadata.st_uid != os.getuid() or metadata.st_mode & 0o022
            or (metadata.st_nlink != 1 and not directory)
            or not (stat.S_ISDIR(metadata.st_mode) if directory else stat.S_ISREG(metadata.st_mode))):
        raise ValueError('foreign or unsafe path refused')
    return metadata


def prepare(version, repo, check=False, check_exact=False):
    if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[1-9][0-9]*)?', version):
        raise ValueError('expected exact release version')
    if not re.fullmatch(r'[A-Za-z0-9-]+/[A-Za-z0-9_.-]+', repo):
        raise ValueError('expected GitHub OWNER/REPO')
    origin = command(['git', 'config', '--get-all', 'remote.origin.url']).decode().strip()
    if origin not in ('https://github.com/' + repo, 'https://github.com/' + repo + '.git',
                      'git@github.com:' + repo + '.git'):
        raise ValueError('origin differs from selected GitHub repository')
    lock = ROOT / '.af/af.lock'
    owned(lock.parent, directory=True)
    metadata = owned(lock)
    with lock.open('rb') as stream:
        before = stream.read(1024 * 1024 + 1)
    if len(before) > 1024 * 1024:
        raise ValueError('lock exceeds byte budget')
    owned(ROOT / 'crates/af/keys/release.pub')
    targets = pin.targets()
    if not targets or len(targets) != len(set(targets)):
        raise ValueError('invalid release target inventory')
    tag = 'v' + version
    archives = {'af-' + tag + '-' + target + '.tar.gz': target for target in targets}
    release = json.loads(command(['gh', 'api', 'repos/' + repo + '/releases/tags/' + tag]))
    if (release['tag_name'] != tag or release['draft'] is not False
            or release['html_url'] != 'https://github.com/' + repo + '/releases/tag/' + tag):
        raise ValueError('release repository/tag/publication differs')
    assets = release['assets']
    names = [asset['name'] for asset in assets]
    expected = set(archives)
    # Published signed releases can have archive-only manifests. Provenance is optional,
    # but its asset and signed row must agree; a declared provenance is never ignored.
    if 'release-provenance.json' in names:
        expected.add('release-provenance.json')
    if (len(names) != len(set(names))
            or set(names) != expected | {'SHA256SUMS', 'SHA256SUMS.minisig', 'install.sh'}
            or any(type(asset['id']) is not int or asset['id'] <= 0 for asset in assets)
            or len({asset['id'] for asset in assets}) != len(assets)):
        raise ValueError('release asset inventory differs')
    with tempfile.TemporaryDirectory(prefix='af-pin-') as scratch:
        scratch = Path(scratch)
        for name in ('SHA256SUMS', 'SHA256SUMS.minisig', *sorted(expected - set(archives))):
            asset = next(asset for asset in assets if asset['name'] == name)
            data = command(['gh', 'api', 'repos/' + repo + '/releases/assets/' + str(asset['id']),
                            '-H', 'Accept: application/octet-stream'], limit=64 * 1024)
            (scratch / name).write_bytes(data)
        command(['minisign', '-V', '-q', '-m', str(scratch / 'SHA256SUMS'),
                 '-p', str(ROOT / 'crates/af/keys/release.pub')])
        signature = (scratch / 'SHA256SUMS.minisig').read_text().splitlines()
        if len(signature) != 4 or signature[2] != 'trusted comment: af ' + tag:
            raise ValueError('signed release version comment differs')
        sums = {}
        for line in (scratch / 'SHA256SUMS').read_text().splitlines():
            match = re.fullmatch(r'([0-9a-f]{64})  (?:\./)?([A-Za-z0-9_.-]+)', line)
            if not match or match[2] in sums:
                raise ValueError('malformed or duplicate signed checksum')
            sums[match[2]] = match[1]
        if set(sums) != expected:
            raise ValueError('signed complete target inventory differs')
        if 'release-provenance.json' in expected:
            provenance_bytes = (scratch / 'release-provenance.json').read_bytes()
            if hashlib.sha256(provenance_bytes).hexdigest() != sums['release-provenance.json']:
                raise ValueError('signed provenance digest differs')
            provenance = json.loads(provenance_bytes)
            if (set(provenance) != {'schema', 'version', 'source', 'repository', 'run_id'}
                    or provenance['schema'] != 'af.release-provenance/1'
                    or provenance['version'] != version or provenance['repository'] != repo
                    or not re.fullmatch('[0-9a-f]{40}', provenance['source'])
                    or not re.fullmatch('[0-9]+', provenance['run_id'])):
                raise ValueError('signed release provenance identity differs')
    text = before.decode('utf-8')
    existing = tomli.loads(text)['af']
    signed_digests = {target: 'sha256:' + sums[name] for name, target in archives.items()}
    if check:
        if existing != {'version': version, 'digests': signed_digests}:
            raise ValueError('existing pin differs from signed release')
        if owned(lock).st_ino != metadata.st_ino or lock.read_bytes() != before:
            raise ValueError('lock changed during verification')
        return
    replacements = {('af',): '[af]\nversion = "' + version + '"\n\n',
                    ('af', 'digests'): '[af.digests]\n' + ''.join(
                        target + ' = "sha256:' + sums[name] + '"\n'
                        for name, target in sorted(archives.items())) + '\n'}
    spans = pin.table_spans(text)
    found = [name for name, _, _ in spans if name in replacements]
    if len(found) != 2 or set(found) != set(replacements):
        raise ValueError('missing or duplicate af tables')
    prepared = ''.join(replacements.get(name, text[start:end]) for name, start, end in spans)
    # The wrapper's post-write guard is additional evidence, never our write authority.
    pin.validate_pin_edit(text, prepared, version, signed_digests)
    after = prepared.encode('utf-8')
    if owned(lock).st_ino != metadata.st_ino or lock.read_bytes() != before:
        raise ValueError('lock changed during preparation')
    if after == before:
        return
    if check_exact:
        raise ValueError('existing pin differs from signed release')
    temporary = None
    try:
        fd, temporary = tempfile.mkstemp(prefix='.af-pin-', dir=lock.parent)
        with os.fdopen(fd, 'wb') as stream:
            os.fchmod(stream.fileno(), stat.S_IMODE(metadata.st_mode))
            stream.write(after)
            stream.flush()
            os.fsync(stream.fileno())
        owned(lock.parent, directory=True)
        if owned(lock).st_ino != metadata.st_ino or lock.read_bytes() != before:
            raise ValueError('lock changed before replacement')
        os.replace(temporary, lock)
        fd = os.open(lock.parent, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    finally:
        if temporary and os.path.exists(temporary):
            os.unlink(temporary)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('version')
    parser.add_argument('--repo', required=True)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument('--check', action='store_true', help='verify existing pin values without writing')
    modes.add_argument('--check-exact', action='store_true', help='verify freshly generated pin bytes without writing')
    args = parser.parse_args()
    try:
        os.chdir(ROOT)
        prepare(args.version, args.repo, args.check, args.check_exact)
    except (ValueError, OSError, KeyError, TypeError, subprocess.TimeoutExpired):
        print('pin-release: signed release pin preparation refused; verify version, repository, '
              'complete assets, trusted signature and owned checkout paths; no pin adoption', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
