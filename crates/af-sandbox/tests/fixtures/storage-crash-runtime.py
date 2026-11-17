# Test-only forwarding transport. ENGINE and BARRIER are supplied by the Rust fixture.
import json
import os
from pathlib import Path
import subprocess
import sys

args = sys.argv[1:]
identity_file = Path(__file__).with_suffix('.worker-id')
if args[:1] == ['create'] and '--name' in args:
    name = args[args.index('--name') + 1]
    if name.endswith('-worker'):
        # Forward every byte, flag, image and the exact private client environment. The
        # sidecar recognizes only this acknowledged workload, never helper identities.
        result = subprocess.run([ENGINE] + args, stdout=subprocess.PIPE)
        identity = result.stdout.strip().decode('ascii', errors='replace')
        if result.returncode == 0 and len(identity) == 64 and all(
                char in '0123456789abcdef' for char in identity):
            identity_file.write_text(json.dumps({'id': identity, 'name': name}))
        sys.stdout.buffer.write(result.stdout)
        sys.exit(result.returncode)
if args[:1] == ['start'] and identity_file.exists():
    owned = json.loads(identity_file.read_text())
    if args[-1] == owned['id']:
        # launch() has synced this full ID before issuing start. Refuse a premature
        # barrier: recovery must have authority to remove the real owned container.
        journal = Path(os.environ['HOME']).parent / (owned['name'] + '.cid')
        assert journal.read_bytes() == owned['id'].encode('ascii')
        with open(BARRIER + '.ready', 'wb') as ready:
            ready.write(b'ready')
        with open(BARRIER + '.release', 'rb') as release:
            release.read(1)
        sys.exit(91)
os.execv(ENGINE, [ENGINE] + args)
