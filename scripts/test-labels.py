#!/usr/bin/env python3
"""Provision only the canonical validation labels; dry-run unless --apply is explicit."""
import argparse
import importlib.util
import json
import shlex
import subprocess
import sys
from pathlib import Path

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('classes', Path(__file__).with_name('test-classes.py'))
tc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tc)


def definitions():
    return [(f'test:{c}', '1D76DB', f'Validate {c}; test:* labels form an explicit class allowlist')
            for c in tc.CLASSES] + [
                (f'no-{c}', 'D93F0B', f'Exclude {c} validation' +
                 (' and exempt separate PR Performance measurement' if c == 'perf' else ''))
                for c in tc.CLASSES] + [
                    ('release:fast', 'FBCA04', 'Default to functional + e2e + uix; skip perf validation'),
                    ('release:full', '0E8A16', 'Require all four validation classes; reject exclusions')]


def commands(repo=None):
    return [['gh', 'label', 'create', name, '--force', '--color', color,
             '--description', description, *(['--repo', repo] if repo else [])]
            for name, color, description in definitions()]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apply', action='store_true')
    parser.add_argument('--repo')
    parser.add_argument('--check', choices=[row[0] for row in definitions()])
    args = parser.parse_args()
    if args.check:
        # This read precedes all release source mutations and remote writes.
        result = subprocess.run(['gh', 'label', 'list', '--search', args.check, '--limit', '100', '--json', 'name',
                                 *(['--repo', args.repo] if args.repo else [])],
                                capture_output=True, text=True)
        if result.returncode or args.check not in [row['name'] for row in json.loads(result.stdout)]:
            sys.exit(f'Missing or unreadable label {args.check}; run make test-labels APPLY=1 '
                     '(or python3 scripts/test-labels.py --apply --repo OWNER/REPO) before release.\n'
                     + result.stderr)
    else:
        for command in commands(args.repo):
            if args.apply:
                subprocess.run(command, check=True)
            else:
                print(shlex.join(command))


if __name__ == '__main__':
    main()
