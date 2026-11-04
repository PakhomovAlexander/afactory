#!/usr/bin/env python3
"""Reclaim unused SDKs only on ephemeral GitHub-hosted Ubuntu validation runners.

No caller-supplied deletion paths or local-gate hook. Keep Docker, Rust, Python, git,
the checkout, runner temporary files and tool caches. This is CI provisioning, not
a change to AF's production 10-GiB free-disk admission floor.
"""
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys


GIB = 1024 ** 3
# Reserve 30 GiB for the Rust build and test scratch, above the production floor.
REQUIRED_FREE = 40 * GIB
# Fixed preinstalled payloads; never derive a removal path from environment variables.
UNUSED_SDKS = (
    Path('/usr/local/lib/android'),
    Path('/usr/share/dotnet'),
    Path('/usr/local/.ghcup'),
    Path('/opt/ghc'),
)


def hosted_linux(env):
    return (platform.system() == 'Linux'
            and env.get('GITHUB_ACTIONS') == 'true'
            and env.get('RUNNER_OS') == 'Linux'
            and env.get('RUNNER_ENVIRONMENT') == 'github-hosted'
            and env.get('ImageOS') in ('ubuntu22', 'ubuntu24', 'ubuntu26'))


def overlaps(left, right):
    return left == right or left in right.parents or right in left.parents


def protected_paths(env):
    protected = [Path.cwd().resolve(), Path(__file__).resolve().parent.parent]
    for name in ('GITHUB_WORKSPACE', 'RUNNER_TEMP', 'HOME', 'RUNNER_TOOL_CACHE'):
        value = env.get(name, '')
        path = Path(value)
        if not value or not path.is_absolute() or not path.is_dir():
            raise ValueError(f'{name} must name an existing absolute directory')
        protected.append(path.resolve())
    for name in ('CARGO_HOME', 'RUSTUP_HOME', 'XDG_CACHE_HOME', 'TMPDIR'):
        if env.get(name):
            path = Path(env[name])
            if not path.is_absolute():
                raise ValueError(f'{name} must be absolute')
            protected.append(path.resolve())
    return protected


def cleanup_paths(protected):
    # Validate the entire list before the first privileged command. Refuse even a
    # dangling symlink, including in ancestors, rather than following it into data.
    present = []
    for path in UNUSED_SDKS:
        if any(parent.is_symlink() for parent in (path, *path.parents)):
            raise ValueError(f'refusing symlink in SDK path: {path}')
        if any(overlaps(path.resolve(), root) for root in protected):
            raise ValueError(f'SDK path overlaps protected state: {path}')
        if path.exists():
            if not path.is_dir() or path.is_mount():
                raise ValueError(f'SDK path is not an ordinary directory: {path}')
            present.append(path)
    return present


def diagnostics(label, roots):
    print(f'CI disk {label}', flush=True)
    subprocess.run(['df', '-h', '--', *map(str, roots)], check=True)
    return {root: shutil.disk_usage(root).free for root in roots}


def prepare(env):
    if not hosted_linux(env):
        print('CI disk preparation skipped: requires an ephemeral GitHub-hosted Ubuntu Linux runner')
        return
    protected = protected_paths(env)
    paths = cleanup_paths(protected)
    roots = list(dict.fromkeys((Path('/'), Path(env['GITHUB_WORKSPACE']), Path(env['RUNNER_TEMP']))))
    before = diagnostics('before SDK cleanup', roots)
    try:
        for path in paths:
            print(f'Removing unused preinstalled SDK: {path}', flush=True)
            subprocess.run(['sudo', '-n', 'rm', '-rf', '--one-file-system', '--', str(path)], check=True)
    finally:
        after = diagnostics('after SDK cleanup', roots)
        for root in roots:
            print(f'{root}: free={after[root] / GIB:.1f} GiB; '
                  f'reclaimed={(after[root] - before[root]) / GIB:.1f} GiB', flush=True)
    if any(free < REQUIRED_FREE for free in after.values()):
        raise ValueError('CI needs at least 40 GiB free before build: '
                         '30 GiB build/test headroom plus the unchanged 10 GiB production floor')


def main():
    try:
        prepare(os.environ)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f'CI disk preparation failed: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
