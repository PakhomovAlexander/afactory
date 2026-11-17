#!/usr/bin/env python3
"""Test-only Engine transport; no flag or fake volume here supplies OS containment.

Actual files, tool hashes, compiler/test output and private volume copies exercise production
admission, dispatch, export and receipt paths. Live container probes own enforcement evidence.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile

args = sys.argv[1:]
volumes = Path(os.environ['HOME']).parent / 'fixture-volumes'
if args[:1] == ['info']:
    print('{"MemoryLimit":true,"SwapLimit":true,"CpuCfsQuota":true,"PidsLimit":true}')
    sys.exit(0)
if args[:2] == ['volume', 'create']:
    (volumes / args[-1]).mkdir(parents=True)
    print(args[-1])
    sys.exit(0)
if args[:2] == ['volume', 'rm']:
    shutil.rmtree(volumes / args[-1])
    sys.exit(0)
if args[:2] == ['image', 'inspect']:
    print('null')
    sys.exit(0)
# Each private client context owns its own daemon records. Creation acknowledges a synced
# full ID but never executes the stored payload. Only exact IDs may start or remove it.
containers = Path(os.environ['HOME']).parent / 'fixture-containers'
containers.mkdir(exist_ok=True)


def valid_id(identity):
    return len(identity) == 64 and all(c in '0123456789abcdef' for c in identity)


def store(identity, record):
    path = containers / (identity + '.json')
    with path.open('w') as stream:
        json.dump(record, stream)
        stream.flush()
        os.fsync(stream.fileno())
    descriptor = os.open(containers, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def load(identity):
    assert valid_id(identity), 'exact full container ID required'
    record = json.loads((containers / (identity + '.json')).read_text())
    assert record['status'] != 'removed', 'container was removed'
    return record


def parse(arguments):
    args = list(arguments)
    mounts = {}
    env = {}
    work = None
    entrypoint = None
    while args and args[0].startswith('-'):
        option = args.pop(0)
        if option in ['--rm', '--init', '--interactive', '--detach', '--pull=never', '--read-only',
                      '--no-healthcheck', '--network=none', '--cap-drop=ALL',
                      '--security-opt=no-new-privileges', '--log-driver=none', '--ipc=none']:
            continue
        if '=' in option:
            option, value = option.split('=', 1)
        else:
            value = args.pop(0)
        if option == '--volume':
            host, guest, mode = value.rsplit(':', 2)
            assert mode == 'ro', 'no Worker or helper receives a writable host bind'
            mounts[guest] = host
        elif option == '--mount':
            fields = dict(part.split('=', 1) for part in value.split(',') if '=' in part)
            assert fields['type'] == 'volume'
            mounts[fields['dst']] = str(volumes / fields['src'] / fields.get('volume-subpath', ''))
        elif option == '-e':
            key, value = value.split('=', 1)
            env[key] = value
        elif option == '--workdir':
            work = value
        elif option == '--entrypoint':
            entrypoint = value
    image = args.pop(0)
    program = entrypoint or args.pop(0)
    assert '@sha256:' in image
    return mounts, env, work, program, args


def execute(arguments):
    mounts, env, work, program, args = parse(arguments)
    if 'af-storage-keeper' in args:
        sys.exit(0)
    if 'af-storage-seed' in args:
        names = args[args.index('af-storage-seed') + 3:]
        disk = Path(mounts['/disk'])
        for name in names:
            shutil.copytree(mounts['/input/' + name], disk / name)
        for name in ('tmp', 'run', 'shm', 'mqueue'):
            (disk / name).mkdir()
        for path in (disk, *disk.rglob('*')):
            path.chmod(path.stat().st_mode | (0o777 if path.is_dir() else 0o666))
        sys.exit(0)
    if program == '/bin/tar':
        names = args[args.index('/disk') + 1:]
        with tarfile.open(fileobj=sys.stdout.buffer, mode='w|', format=tarfile.GNU_FORMAT,
                          dereference=True) as archive:
            for name in names:
                archive.add(Path(mounts['/disk']) / name, arcname=name)
        sys.exit(0)
    if work == '/':
        assert not mounts, 'image admission must see no source mounts'
        assert args[0] == '-c' and 'test -L' in args[1]
        sys.exit(0)
    assert '--interactive' in arguments
    assert env['HOME'].startswith('/af/state/')


    def local(value):
        for guest, host in sorted(mounts.items(), key=lambda pair: -len(pair[0])):
            if value == guest or value.startswith(guest + '/'):
                return host + value[len(guest):]
        return value


    if program == '/usr/bin/sha256sum':
        assert set(mounts) == {'/work', '/af/package'}
        assert not list(Path(local(work)).iterdir()), 'preflight must not see project bytes'
        for value in args:
            print(hashlib.sha256(Path(value).read_bytes()).hexdigest() + '  ' + value)
        sys.exit(0)
    env = {key: local(value) for key, value in env.items()}
    # Fixed fixtures only. This transport never runs untrusted Worker code or model tools.
    result = subprocess.run([program] + [local(arg) for arg in args], cwd=local(work), env=env,
                            stdin=sys.stdin.buffer)
    sys.exit(result.returncode)


operation = args.pop(0)
if operation == 'create':
    parse(args)
    assert '--name' in args
    name = args[args.index('--name') + 1]
    for path in containers.glob('*.json'):
        existing = json.loads(path.read_text())
        assert existing['name'] != name or existing['status'] == 'removed', 'name already owned'
    identity = os.urandom(32).hex()
    store(identity, {'name': name, 'argv': args, 'status': 'created'})
    print(identity)
elif operation == 'start':
    assert args[:-1] in ([], ['--attach'], ['--attach', '--interactive'])
    identity = args[-1]
    record = load(identity)
    assert record['status'] in ('created', 'exited'), 'container already running'
    record['status'] = 'running'
    store(identity, record)
    code = 1
    try:
        execute(record['argv'])
    except SystemExit as result:
        code = result.code
    finally:
        # The fixed keeper models a detached live helper; other commands really finish.
        if 'af-storage-keeper' not in record['argv']:
            record.update(status='exited', exit_code=code)
            store(identity, record)
    sys.exit(code)
elif operation == 'ps':
    assert len(args) == 5 and args[:4] == ['--all', '--no-trunc', '--quiet', '--filter']
    assert args[4].startswith('id=')
    identity = args[4][3:]
    assert valid_id(identity), 'exact full container ID required'
    path = containers / (identity + '.json')
    if path.exists() and json.loads(path.read_text())['status'] != 'removed':
        print(identity)
elif operation == 'rm':
    assert len(args) == 2 and args[0] == '-f'
    identity = args[1]
    record = load(identity)
    record['status'] = 'removed'
    store(identity, record)
else:
    raise AssertionError('unsupported runtime operation: ' + operation)
