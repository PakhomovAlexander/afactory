"""Required-gate, real-AF namespace regression; no model, login or live host access.

The single-UID sandbox cannot create a second mapped owner. Only the launcher's stat
of the fixture ancestor models root ownership; mode 0111 enforces actual search-only
access for its non-root owner, and the reported root mode is 0711. No child AF stat,
account lookup, authority or ownership check is mocked. All mounts/writes are private.
"""
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
from unittest import mock

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/performance'))
from perflab import account_isolation, telemetry_lab  # noqa: E402


@contextmanager
def foreign_ancestor(parent, mode):
    original = Path.stat
    original_mode = stat.S_IMODE(original(parent).st_mode)
    # Unlike an owner-owned 0711 directory, this really denies listing by the caller.
    parent.chmod(0o111 if mode == 0o711 else mode)

    def foreign(path, *args, **kwargs):
        info = original(path, *args, **kwargs)
        if path == parent:
            values = list(info)
            values[4] = 0
            values[0] = (values[0] & ~0o7777) | mode
            return os.stat_result(values)
        return info

    try:
        with mock.patch.object(Path, 'stat', foreign):
            yield
    finally:
        parent.chmod(original_mode)


def identity(path):
    info = path.stat()
    volume = os.statvfs(path)
    return [info.st_dev, info.st_ino, stat.S_IMODE(info.st_mode),
            volume.f_blocks, volume.f_frsize, hashlib.sha256(path.read_bytes()).hexdigest()]


def child(case, af, parent_mode):
    assert os.geteuid() != 0, 'the actual AF child must run as a non-root UID'
    assert account_isolation.in_private_account_namespace()
    assert account_isolation.FIXTURE not in os.environ
    assert stat.S_IMODE(case.parent.stat().st_mode) == parent_mode
    assert case.parent.stat().st_uid == os.geteuid()
    assert Path.cwd() == case / 'cwd'
    for path, expected in json.loads((case / 'identities.json').read_text()):
        assert identity(Path(path)) == expected, path
    assert (case / 'source-link').resolve() == case / 'source'
    assert Path(os.environ['AF_WORKSPACE_ROOT']).joinpath('fixtures/telemetry-bench').is_dir()

    def run(args, **kwargs):
        result = subprocess.run(args, capture_output=True, text=True, **kwargs)
        assert result.returncode == 0, (args, result.returncode, result.stdout, result.stderr)
        return result

    inventory = json.loads(run([af, 'storage', '--json']).stdout)
    assert str(case / 'os-account/.local/state/af/task/local') in json.dumps(inventory), inventory
    # Real native provider selection and its exact zero-Attempt missing-provider receipt.
    # The production smoke validates every receipt field; it performs no provider call.
    run(['bash', str(ROOT / 'scripts/release-smoke.sh'), af, str(case / 'release')])
    # Also compile an actual scripted-provider Task with the built binary, as telemetry
    # does before its measured interval. Preserve the final-child open tripwire through
    # the guarded environment; no debug account override is supplied.
    overhead = telemetry_lab.script_module('scripts/telemetry-overhead.py')
    fixture = 'telemetry-bench-concurrent'
    repo = case / 'task-repo'
    overhead.copy_writable(ROOT / 'fixtures/telemetry-bench' / fixture / 'repo', repo)
    for args in (['init', '-q'], ['add', '-A'],
                 ['-c', 'user.name=fixture', '-c', 'user.email=fixture@example.invalid',
                  'commit', '-qm', 'private fixture']):
        run(['git', '-C', str(repo), *args])

    def environment(run_dir):
        env = overhead.private_env(run_dir, 'off')
        env.update({name: os.environ[name] for name in (
            'LD_PRELOAD', 'WAKE_FIXTURE_ACCOUNT_MODE', 'WAKE_FIXTURE_FORBID_OPEN')})
        return env

    overhead.admit_fixture(af, fixture, repo, environment=environment)
    print('non-root namespace: inventory, native Provider refusal and ready Task:', case.name)


def exercise(root, af, resolver, family):
    assert os.geteuid() != 0, 'required regression must exercise a non-root AF'
    for name, mode, populated in (('ordinary', 0o755, False),
                                 ('search-only', 0o711, False),
                                 ('populated', 0o755, True)):
        case = root / family / name
        parent = case.parent
        # Rust seeds this actual account authority with valid CAS/SQLite state.
        assert (case / 'os-account/.local/state/af/task/local').is_dir()
        for directory in ('home', 'config', 'state', 'data', 'cache', 'tmp', 'cwd', 'bin'):
            (case / directory).mkdir(mode=0o700)
        (case / 'source').write_text('original source bytes\n')
        (case / 'source').chmod(0o444)
        (case / 'source-link').symlink_to('source')
        (case / 'bin/af-link').symlink_to(af)
        required = [case / 'source', Path(af), Path(resolver)]
        (case / 'identities.json').write_text(json.dumps(
            [(str(path), identity(path)) for path in required]))
        if populated:
            # Real ambient entries: the previous algorithm exceeded ARG_MAX.
            for index in range(6000):
                (parent / ('unrelated-%04d-' % index + 'x' * 210)).touch()
        env = {name: value for name, value in os.environ.items()
               if not name.startswith(('AF_', 'XDG_', 'GIT_', 'PERFLAB_'))}
        env.update(HOME=str(case / 'home'), XDG_CONFIG_HOME=str(case / 'config'),
                   XDG_STATE_HOME=str(case / 'state'), XDG_DATA_HOME=str(case / 'data'),
                   XDG_CACHE_HOME=str(case / 'cache'), TMPDIR=str(case / 'tmp'),
                   AF_WORKSPACE_ROOT=str(ROOT), AF_SELF_OFFLINE='1',
                   AF_PROVIDERS_FILE=str(case / 'providers.toml'),
                   LD_PRELOAD=resolver, WAKE_FIXTURE_ACCOUNT_MODE='passthrough',
                   WAKE_FIXTURE_FORBID_OPEN=str(root / 'external-account'),
                   PATH=str(case / 'bin') + ':/usr/local/bin:/usr/bin:/bin',
                   GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1')
        command = [sys.executable, '-I', '-B', str(Path(__file__).resolve()), '--child',
                   str(case), str(case / 'bin/af-link'), str(mode)]
        with foreign_ancestor(parent, mode):
            if mode == 0o711:
                try:
                    os.listdir(parent)
                except PermissionError:
                    pass
                else:
                    raise AssertionError('search-only fixture permitted sibling listing')
            # env/cwd, executable symlinks and runtime fixture roots are separate
            # declared inputs. No unrelated entry may become a mount argument.
            argv = account_isolation.namespace_argv(case / 'os-account', command,
                                                   env=env, cwd=case / 'cwd')
            assert sum(len(os.fsencode(arg)) + 1 for arg in argv) < 65536
            assert not any('unrelated-' in arg for arg in argv)
            result = subprocess.run(argv, env=env, cwd=case / 'cwd',
                                    capture_output=True, text=True)
            assert result.returncode == 0, (family, name, result.stdout, result.stderr)
            print(result.stdout, end='', flush=True)


if __name__ == '__main__':
    if sys.argv[1] == '--child':
        child(Path(sys.argv[2]), sys.argv[3], int(sys.argv[4]))
    else:
        exercise(Path(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4])
