#!/usr/bin/env python3
"""Keep nextest reports outside sealed sources when verify.sh selects a target."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    command = ['cargo', 'nextest', 'run', *sys.argv[1:]]
    target = os.environ.get('AF_GATE_NEXTEST_TARGET')
    if not target:
        return subprocess.call(command)
    # A unique private directory avoids concurrent report/config collisions. Reports
    # survive failure for diagnosis; only the temporary tool configuration is removed.
    reports = Path(target).resolve() / 'nextest-reports'
    reports.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='run-', dir=reports))
    config = run / 'store.toml'
    try:
        config.write_text('[store]' + chr(10) + 'dir = ' + json.dumps(str(run / 'store')) + chr(10))
        print(f'nextest gate reports: {run / "store"}', flush=True)
        return subprocess.call(command + ['--tool-config-file', f'af-gate:{config}'])
    finally:
        config.unlink(missing_ok=True)


if __name__ == '__main__':
    code = main()
    sys.exit(code if code >= 0 else 128 - code)
