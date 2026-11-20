#!/usr/bin/env bash
# Provision the disposable Ubuntu release runner, then prove the real account namespace.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

sudo apt-get update
sudo apt-get install -y --no-install-recommends bubblewrap apparmor-profiles

# Ubuntu 24.04 ships this opt-in profile in apparmor-profiles. It admits bwrap's
# namespace setup, then denies capabilities to its children. Keep the global userns
# restriction and refuse to replace a different pre-existing application policy.
packaged=/usr/share/apparmor/extra-profiles/bwrap-userns-restrict
active=/etc/apparmor.d/bwrap-userns-restrict
if sudo test -e "$active"; then
  sudo cmp -- "$packaged" "$active"
else
  sudo install -m 0644 -- "$packaged" "$active"
fi
sudo apparmor_parser -r -- "$active"

# Run without sudo: testing root would hide the admission failure we need to detect.
python3 -I -B - <<'PY'
from pathlib import Path
import os
import subprocess
import sys
import tempfile

root = Path.cwd()
sys.path.insert(0, str(root / 'scripts/performance'))
from perflab.account_isolation import namespace_argv

assert os.geteuid() != 0, 'release namespace preflight must run unprivileged'
with tempfile.TemporaryDirectory(prefix='af-release-namespace-') as directory:
    home = Path(directory) / 'home'
    home.mkdir(mode=0o700)
    env = {**os.environ, 'HOME': str(home)}
    code = '''
from pathlib import Path
import os, pwd, sys
sys.path.insert(0, str(Path.cwd() / 'scripts/performance'))
from perflab.account_isolation import in_private_account_namespace
assert in_private_account_namespace(), 'private account database was not mounted'
assert pwd.getpwuid(os.geteuid()).pw_dir == os.environ['HOME'] == sys.argv[1]
assert os.readlink('/proc/self/ns/user') != sys.argv[2], 'user namespace was not created'
print('release private account namespace: PASS')
'''
    command = [sys.executable, '-I', '-B', '-c', code, str(home),
               os.readlink('/proc/self/ns/user')]
    with namespace_argv(home, command, env=env, cwd=root) as argv:
        subprocess.run(argv, env=env, cwd=root, check=True)
PY
