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
import tempfile
import time
from unittest import mock

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/performance'))
from perflab import account_isolation, telemetry_exec, telemetry_lab  # noqa: E402


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


def volume(path):
    info = os.statvfs(path)
    return [Path(path).stat().st_dev, info.f_blocks, info.f_frsize]


def child(case, af, parent_mode):
    assert os.geteuid() != 0, 'the actual AF child must run as a non-root UID'
    assert account_isolation.in_private_account_namespace()
    assert account_isolation.FIXTURE not in os.environ
    assert stat.S_IMODE(case.parent.stat().st_mode) == parent_mode
    assert case.parent.stat().st_uid == os.geteuid()
    assert Path.cwd() == case / 'cwd'
    for path, expected in json.loads((case / 'identities.json').read_text()):
        assert identity(Path(path)) == expected, path
    for path, expected in json.loads((case / 'volumes.json').read_text()):
        assert volume(path) == expected, (path, volume(path), expected)
    assert (case / 'source-link').resolve() == case / 'source'
    assert Path(os.environ['AF_WORKSPACE_ROOT']).joinpath('fixtures/telemetry-bench').is_dir()

    def run(args, **kwargs):
        result = subprocess.run(args, capture_output=True, text=True, **kwargs)
        assert result.returncode == 0, (args, result.returncode, result.stdout, result.stderr)
        return result

    inventory = json.loads(run([af, 'storage', '--json']).stdout)
    assert str(case / 'os-account/.local/state/af/task/local') in json.dumps(inventory), inventory
    for measured in inventory['volumes']:
        statvfs = os.statvfs(measured['root'])
        # These are actual AF measurements, bounded by that path's actual capacity;
        # replacing a small temporary volume with HOME's disk cannot satisfy this.
        assert measured['error'] is None, measured
        assert 0 <= measured['free_bytes'] <= statvfs.f_blocks * statvfs.f_frsize, measured
        assert measured['below_floor'] == (measured['free_bytes'] < inventory['min_free_bytes'])
    tmp_inventory = json.loads(run([af, 'storage', '--json'],
                                   env=dict(os.environ, TMPDIR='/tmp')).stdout)
    sandbox = next(v for v in tmp_inventory['volumes'] if v['role'] == 'sandbox')
    assert sandbox['root'] == '/tmp' and sandbox['error'] is None, sandbox
    tmp_stat = os.statvfs('/tmp')
    assert 0 <= sandbox['free_bytes'] <= tmp_stat.f_blocks * tmp_stat.f_frsize, sandbox
    assert Path(sandbox['volume']).stat().st_dev == Path('/tmp').stat().st_dev, sandbox
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


def parsed_measurements(root, af, resolver):
    """Public supported syntax plus actual scripts/executor, outside any private NSS mount.

    The AF alias and corpus are outside cwd/HOME/PATH/source/other declared branches.
    Scripts must use their parsed resources rather than recover paths from argv tokens.
    All scratch has private bounded lifetime; measurements retain their original argv.
    """
    with tempfile.TemporaryDirectory(prefix='af-declared-inputs-', dir='/tmp') as inputs, \
            tempfile.TemporaryDirectory(prefix='af-declared-corpus-', dir='/tmp') as corpus_root, \
            tempfile.TemporaryDirectory(prefix='af-declared-outputs-', dir='/tmp') as outputs, \
            tempfile.TemporaryDirectory(prefix='namespace-work-', dir=ROOT / 'target') as work:
        inputs, outputs, work = Path(inputs), Path(outputs), Path(work)
        binary = inputs / 'tools/af'
        binary.parent.mkdir()
        binary.symlink_to(af)
        held = identity(Path(af))
        home = outputs / 'home'
        home.mkdir(mode=0o700)
        env = {name: value for name, value in os.environ.items()
               if not name.startswith(('AF_', 'XDG_', 'GIT_', 'PERFLAB_'))}
        env.update(HOME=str(home), TMPDIR=str(outputs), LD_PRELOAD=resolver,
                   WAKE_FIXTURE_ACCOUNT_MODE='passthrough',
                   WAKE_FIXTURE_FORBID_OPEN=str(root / 'external-account'),
                   GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1',
                   PATH='/usr/local/bin:/usr/bin:/bin')
        cwd = Path('/tmp')

        def call(script, arguments):
            result = subprocess.run([sys.executable, '-I', '-B', str(ROOT / 'scripts' / script),
                                     *arguments], cwd=cwd, env=env, capture_output=True, text=True)
            assert result.returncode == 0, (script, arguments, result.stdout, result.stderr)
            return json.loads(result.stdout)

        corpus = Path(corpus_root) / 'corpus'
        call('telemetry-corpus.py', ['generate', '--out', str(corpus), '--runs', '2', '--jobs', '1'])
        for joined in (False, True):
            # One form is joined absolute; the other is standalone relative, including
            # parent components under a reconstructed foreign cwd.
            relative = '../tmp/' + str(binary.relative_to('/tmp'))
            af_args = ['--af=' + str(binary)] if joined else ['--af', relative]
            for script in ('telemetry-overhead.py', 'telemetry-invocation-cost.py', 'telemetry-corpus.py'):
                label = script + ('-joined' if joined else '-relative')
                report = outputs / (label + '.json')
                arguments = af_args + ['--report=' + str(report), '--json']
                if script == 'telemetry-corpus.py':
                    arguments = ['measure', '--dir', os.path.relpath(corpus, cwd),
                                 '--repeat', '1', *arguments]
                else:
                    arguments += ['--work=' + str(work / label), '--pairs', '1', '--scale', 'smoke']
                    if script == 'telemetry-overhead.py':
                        arguments += ['--fixture', 'telemetry-bench-concurrent']
                document = call(script, arguments)
                assert json.loads(report.read_text()) == document
                assert document['af']['path'] == str(Path(af).resolve()), document
                assert identity(Path(af)) == held

        # The local laboratory already has parsed readable/writable metadata. Exercise
        # its actual executor with a joined option, and dynamically created run resources.
        registry, _ = telemetry_lab.load_registry()
        capture = outputs / 'capture'
        capture.mkdir()
        env[account_isolation.FIXTURE] = str(home)
        command = [sys.executable, '-I', '-B', str(ROOT / 'scripts/telemetry-overhead.py'),
                   '--af=' + str(binary), '--pairs', '1', '--scale', 'smoke',
                   '--fixture', 'telemetry-bench-concurrent', '--work=' + str(work / 'local'), '--json']
        execution = telemetry_exec.Execution(command, env, str(cwd), [str(work)],
            telemetry_lab._readable(registry, str(ROOT), 'overhead', str(binary)),
            str(capture), 'local', 1 << 30, time.monotonic() + 120, telemetry_lab.FILE_BYTES_MAX)
        outcome = telemetry_exec.TrustedLocal(sys.executable).run(execution)
        assert outcome.failure() is None and outcome.code == 0, (outcome.failure(), outcome.stderr)
        assert json.loads(outcome.stdout)['fixtures'][0]['pairs'][0]['off']['exit_code'] == 0
        assert identity(Path(af)) == held
        print('parsed namespace inputs: all scripts, both AF forms and actual local executor', flush=True)


def exercise(root, af, resolver, family):
    assert os.geteuid() != 0, 'required regression must exercise a non-root AF'
    for name, mode, populated in (('ordinary', 0o755, False),
                                 ('search-only', 0o711, False),
                                 ('populated', 0o755, True)):
        case = root / family / name
        parent = case.parent
        # Rust seeds this actual account authority with valid CAS/SQLite state.
        assert (case / 'os-account/.local/state/af/task/local').is_dir()
        if family == 'split-tmp':
            assert Path('/tmp').stat().st_dev != case.stat().st_dev
        for directory in ('home', 'config', 'state', 'data', 'cache', 'tmp', 'cwd', 'bin'):
            (case / directory).mkdir(mode=0o700)
        (case / 'source').write_text('original source bytes\n')
        (case / 'source').chmod(0o444)
        (case / 'source-link').symlink_to('source')
        (case / 'bin/af-link').symlink_to(af)
        required = [case / 'source', Path(af), Path(resolver)]
        (case / 'identities.json').write_text(json.dumps(
            [(str(path), identity(path)) for path in required]))
        (case / 'volumes.json').write_text(json.dumps(
            [(str(path), volume(path)) for path in (Path('/tmp'), case / 'tmp', case / 'source')]))
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
            with account_isolation.namespace_argv(
                    case / 'os-account', command, env=env, cwd=case / 'cwd',
                    inputs=(Path(__file__).resolve(), case, af, resolver)) as argv:
                assert sum(len(os.fsencode(arg)) + 1 for arg in argv) < 65536
                assert not any('unrelated-' in arg for arg in argv)
                result = subprocess.run(argv, env=env, cwd=case / 'cwd',
                                        capture_output=True, text=True)
                assert result.returncode == 0, (family, name, result.stdout, result.stderr)
            print(result.stdout, end='', flush=True)
    if family == 'tmp':
        parsed_measurements(root, af, resolver)


if __name__ == '__main__':
    if sys.argv[1] == '--child':
        child(Path(sys.argv[2]), sys.argv[3], int(sys.argv[4]))
    else:
        exercise(Path(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4])
