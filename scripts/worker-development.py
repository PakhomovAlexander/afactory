#!/usr/bin/env python3
"""Render a pinned development companion from an operator-owned, reviewed machine binding.

This command neither installs dependencies nor runs a project or container. The kernel still
admits the exact binding and verifies tools before it dispatches a Worker.
"""
import argparse
import hashlib
import json
import os
import stat
from pathlib import Path
import re
import sys
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent / "performance"))
from perflab._vendor import tomli as tomllib

DOMAIN = b'review.kernel/content-id/v1\0'
DIGEST = re.compile(r'sha256:[0-9a-f]{64}\Z')
NAME = re.compile(r'[A-Za-z0-9][A-Za-z0-9_-]{0,127}\Z')


def parent_descriptor(path):
    path = path.absolute()
    if '..' in path.parts:
        raise ValueError('parent traversal refused')
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    descriptor = os.open(path.anchor, flags)
    try:
        for part in path.parts[1:-1]:
            child = os.open(part, flags, dir_fd=descriptor)
            os.close(descriptor)
            descriptor = child
        return descriptor, path.name
    except BaseException:
        os.close(descriptor)
        raise


def regular_bytes(path, limit):
    parent, name = parent_descriptor(path)
    try:
        descriptor = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
        with os.fdopen(descriptor, 'rb') as source:
            if not stat.S_ISREG(os.fstat(source.fileno()).st_mode):
                raise ValueError(f'not a regular file: {path}')
            data = source.read(limit + 1)
    finally:
        os.close(parent)
    if len(data) > limit:
        raise ValueError(f'input exceeds {limit} bytes')
    return data


def write_absent(path, text):
    parent, name = parent_descriptor(path)
    try:
        descriptor = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                             0o600, dir_fd=parent)
        with os.fdopen(descriptor, 'w', encoding='utf-8') as output:
            output.write(text)
    finally:
        os.close(parent)


def binding(path):
    value = tomllib.loads(regular_bytes(path, 65536).decode())
    required = {'schema', 'runtime', 'image', 'checksum_program', 'probe_shell', 'tools', 'limits', 'storage'}
    if (not required <= set(value) or set(value) - required - {'dependency_sources', 'cache_limits'}
            or value['schema'] != 'af.worker-development-binding/3'):
        raise ValueError('unsupported machine development binding schema or fields')
    if not re.fullmatch(r'[^\s@-][^\s@]*@sha256:[0-9a-f]{64}', value['image']):
        raise ValueError('machine tool image must be digest-pinned')
    if not 1 <= len(value['tools']) <= 32:
        raise ValueError('machine binding needs one to 32 tools')
    for name, tool in value['tools'].items():
        if (not NAME.fullmatch(name) or set(tool) != {'program', 'identity'}
                or not DIGEST.fullmatch(tool['identity'])):
            raise ValueError('invalid machine tool declaration')
    for program in [value['runtime'], value['checksum_program'], value['probe_shell'],
                    *(tool['program'] for tool in value['tools'].values())]:
        if (not program.startswith('/') or any(part in ('', '.', '..')
                                             for part in program[1:].split('/'))
                or any(ord(char) < 32 for char in program) or '\\' in program):
            raise ValueError('machine programs need canonical absolute paths')
    limits = value['limits']
    if (set(limits) != {'wall_ms', 'output_bytes', 'cpu_millicores', 'memory_bytes', 'processes', 'disk_bytes'}
            or any(type(bound) is not int or not 1 <= bound <= 9007199254740991
                   for bound in limits.values())
            or limits['wall_ms'] > 14400000 or limits['output_bytes'] > 1048576
            or not 4096 <= limits['disk_bytes'] <= 8589934592
            or limits['disk_bytes'] % 4096 != 0 or value['storage'] != 'tmpfs_volume'):
        raise ValueError('invalid machine limits or unsupported aggregate storage backend')
    sources = value.get('dependency_sources', {})
    if (not isinstance(sources, dict) or len(sources) > 32
            or any(not NAME.fullmatch(key) or not DIGEST.fullmatch(item)
                   for key, item in sources.items())):
        raise ValueError('invalid approved dependency sources')
    if not sources:
        value.pop('dependency_sources', None)
    if 'cache_limits' in value:
        ceilings = {'max_bytes': 8589934592, 'max_entries': 500000,
                    'max_depth': 64, 'max_path_bytes': 4096}
        cache_limits = value['cache_limits']
        if (set(cache_limits) != set(ceilings)
                or any(type(bound) is not int or not 1 <= bound <= ceilings[key]
                       for key, bound in cache_limits.items())):
            raise ValueError('invalid cache ceilings')
    # Keys in this closed contract are ASCII, including symbolic tool names, so this is the
    # kernel canonicalizer's UTF-16 key order. All numbers are positive exact integers.
    canonical = json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(',', ':')).encode()
    identity = 'sha256:' + hashlib.sha256(DOMAIN + canonical).hexdigest()
    return value, identity


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binding', required=True, type=Path)
    parser.add_argument('--template', type=Path)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    if (args.template is None) != (args.output is None):
        parser.error('--template and --output are supplied together')
    try:
        value, identity = binding(args.binding)
        if args.template:
            text = regular_bytes(args.template, 65536).decode()
            text = text.replace('@BINDING_ID@', identity)
            for name, tool in value['tools'].items():
                text = text.replace('@' + name.upper() + '_ID@', tool['identity'])
            if re.search(r'@[A-Z0-9_-]+@', text):
                raise ValueError('template requests a tool missing from the machine binding')
            request = tomllib.loads(text)
            if request.get('schema') != 'af.worker-development/3':
                raise ValueError('unsupported Worker development template schema')
            disk_bytes = request.get('limits', {}).get('disk_bytes')
            if (type(disk_bytes) is not int or not 4096 <= disk_bytes <= value['limits']['disk_bytes']
                    or disk_bytes % 4096 != 0):
                raise ValueError('Worker template requires an aggregate disk bound within machine policy')
            # Descriptor-relative exclusive creation: aliases and stale output are refused.
            write_absent(args.output, text)
        print(identity)
        return 0
    except (OSError, ValueError, TypeError, KeyError) as error:
        print(f'Worker development: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
