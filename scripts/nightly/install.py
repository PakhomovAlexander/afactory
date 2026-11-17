#!/usr/bin/python3 -IB
"""Offline prepare/install of pinned nightly authority; never register, enable or publish."""
import argparse
import json
import os
from pathlib import Path
import re
import platform
import pwd
import shutil
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path[:0] = [str(HERE), str(ROOT / 'scripts/performance')]
import scenario as runtime  # noqa: E402
from perflab import executor, git_refs  # noqa: E402
from perflab.values import canonical_digest, document_bytes, require, sha256_bytes  # noqa: E402

LIMITS = dict(max_tokens=0, max_wall_ms=72000000, max_dispatches=2048)
# Full native comparison reserve on the provisioned VM, before nightly build space.
MIN_FREE = 130 * 1024**3


def literal(kind, value):
    return dict(source='literal', value=dict(type=kind, value=value))


def bundle(digest, repository):
    tag = dict(source='input', port='nightly_tag')
    subject = dict(source='project', value=tag, projection=dict(kind='nightly_tag_text'))
    revision = literal('text', digest)
    grants, steps = [], []
    for index, op in enumerate(runtime.OPS):
        grant = dict(adapter='nightly', operation=op, destination='github:' + repository)
        grants.append(grant)
        inputs = dict(subject=subject, revision=revision, nightly_tag=tag)
        if index:
            inputs['evidence'] = dict(source='step', step='wait_' + str(index-1), port='result')
        steps.append(dict(id='submit_' + str(index), inputs=inputs, outputs=dict(result='action'), when=None,
            max_attempts=1, timeout_ms=30000, on_timeout='fail', operation=dict(kind='action', grant=grant,
            requires_decision=False, required_condition=None if index == 0 else runtime.CONDITIONS[runtime.OPS[index-1]])))
        observe = dict(adapter='nightly', operation='observe', destination=grant['destination'])
        steps.append(dict(id='wait_' + str(index), inputs=dict(subject=subject, revision=revision),
            outputs=dict(result='observation'), when=None, max_attempts=1, timeout_ms=LIMITS['max_wall_ms'],
            on_timeout='fail', operation=dict(kind='wait', grant=observe, condition=runtime.CONDITIONS[op])))
    grants.append(observe)
    final = dict(source='step', step='wait_3', port='result')
    return dict(schema='af/scenario-bundle@9', root='nightly-release', definitions={'nightly-release': dict(
        schema='af/scenario@9', inputs=dict(nightly_tag='nightly_tag'), outputs=dict(release='observation'),
        steps=steps, result=dict(release=final), completion=[dict(source='project', value=final,
            projection=dict(kind='observation_satisfied'))], admission=None,
        authority=dict(task_plans=[], task_templates=[], grants=grants, decision_owners=[]), limits=LIMITS)})


def ant(definition, start):
    return dict(schema='af/ant@10', id='nightly-release', scenario=definition, inputs={},
        occurrence_inputs=dict(nightly_tag='scheduled_nightly_tag'),
        schedule=dict(kind='daily', timezone='Europe/Moscow', hour=3, minute=0, start_unix_ms=start), owner='owner',
        notification='local:nightly-operator-journal', execution=dict(overlap='skip', missed='skip',
            lateness_ms=600000, per_run=LIMITS, rolling_tokens=0, rolling_window_ms=172800000))


def units(af, repo, state, installation, user):
    common = ('[Service]\nType=oneshot\nUser=' + user + '\nUMask=0077\nEnvironment=PYTHONDONTWRITEBYTECODE=1\n'
              'WorkingDirectory=' + repo + '\nKillMode=control-group\nTimeoutStopSec=120\n')
    conflicts = ('Conflicts=af-nightly.timer af-nightly-build.service af-nightly-measure.service '
                 'af-nightly-report.service af-nightly-publish.service\n\n')
    tick = ('[Unit]\nDescription=Tick native AF Ants\n' + conflicts + common +
            'ExecStart=' + installation + '/tick\nTimeoutStartSec=120\n')
    worker = ('[Unit]\nDescription=External durable native nightly worker\n' + conflicts + common +
              'ExecStart=' + installation + '/worker\nTimeoutStartSec=20h\n'
              'PrivateTmp=no\nPrivateMounts=no\nTasksMax=2048\nMemoryMax=16G\nLimitNOFILE=4096\n'
              '# Optional existing passphrase, outside the repository:\n'
              '#LoadCredential=af-nightly-sign-password:/etc/af-nightly/sign-password\n')

    def timer(service):
        return ('[Unit]\nDescription=Drive ' + service + '\n\n[Timer]\nOnBootSec=60\n'
                'OnUnitInactiveSec=60\nAccuracySec=1s\nUnit=' + service + '\n\n[Install]\nWantedBy=timers.target\n')
    return {'af-nightly-ant-tick.service': tick, 'af-nightly-worker.service': worker,
            'af-nightly-ant-tick.timer': timer('af-nightly-ant-tick.service'),
            'af-nightly-worker.timer': timer('af-nightly-worker.service')}


def safe_path(value):
    require(isinstance(value, str) and re.fullmatch(r'/[A-Za-z0-9_./-]+', value)
            and Path(value).is_absolute() and '..' not in Path(value).parts and str(Path(value)) == value,
            'unsafe deployment path')
    return value


def write_new(path, value, executable=False):
    with path.open('xb') as stream:
        stream.write(value.encode() if isinstance(value, str) else value)
        stream.flush()
        os.fsync(stream.fileno())
    path.chmod(0o700 if executable else 0o600)


def authority_paths():
    return sorted(set([*HERE.glob('*.py'), *(ROOT / 'scripts/performance').rglob('*.py'),
                       *(ROOT / 'schemas').glob('*.json'), ROOT / 'crates/af/keys/nightly.pub']))


def immutable_path(path):
    path = Path(path)
    require(path.is_absolute() and path.resolve() == path, 'immutable dependency contains an alias or link')
    for entry in [path, *path.parents]:
        info = entry.lstat()
        require(info.st_uid == 0 and not info.st_mode & 0o022 and not entry.is_symlink(),
                'control dependency must be root owned and immutable to the worker')


def immutable_control(config, repo):
    require(Path(repo) == ROOT and ROOT.resolve() == ROOT, 'installer must run from the configured immutable control')
    git = git_refs.Git(repo, trusted_repository=True)
    require(git.resolve('HEAD', 'control checkout') == config['control_sha'], 'control checkout differs from exact control_sha')
    executor.producer(git, config['control_sha'], installed=str(ROOT))
    for path in [ROOT, *authority_paths()]:
        immutable_path(path)
        if path.is_file():
            require(git.read_file(config['control_sha'], str(path.relative_to(ROOT))) == path.read_bytes(),
                    'installed authority is not the exact reviewed committed control')


def dependency_errors(args, run=subprocess.run):
    errors = []
    if (platform.system(), platform.machine()) != ('Linux', 'x86_64'):
        errors.append('Linux x86_64 worker host')
    for command in ('git', 'docker', 'minisign', 'python3'):
        if shutil.which(command) is None:
            errors.append(command + ' executable')
    if not Path('/usr/share/zoneinfo/Europe/Moscow').is_file():
        errors.append('Europe/Moscow tzdata')
    try:
        import build
        require(pwd.getpwnam(args.user).pw_uid == os.getuid(), 'installer must run as the configured worker account')
        config = build.load_config(args.config)
        runtime.private(args.config)
        runtime.private(config['report']['executor_config'])
        lab = executor.load_config(config['report']['executor_config'])
        require(lab['backend'] == 'local' and lab['core']['tier'] == 'nightly' and not lab['core']['cases']
                and lab['repository'] == config['repository'], 'executor must select full native nightly family')
        immutable_control(lab, args.repo)
        immutable_path(args.af)
        for key in ('sign_key', 'public_key'):
            # Stat only: never read the signing secret in deployment tooling.
            require(Path(config[key]).resolve() == Path(config[key]), 'signing path contains links')
            require(Path(config[key]).is_file(), 'missing signing prerequisite: ' + key)
        runtime.private(config['sign_key'])
        runtime.trusted_key(config['public_key'])
        for path in [config['publish']['github_token_file'], config.get('github_token_file'),
                     config['publish'].get('telegram_token_file')]:
            if path:
                runtime.private(path)
        require(Path(config['qemu_aarch64']).is_file() and os.access(config['qemu_aarch64'], os.X_OK), 'missing QEMU executable')
        require(Path(config['mirror']).is_dir(), 'missing source mirror')
        require(config['mirror'] == lab['repository_path'], 'build and comparison must share the trusted mirror')
        for directory in (config['builds_root'], lab['root'], args.output, args.root, args.state):
            existing = Path(directory)
            while not existing.exists():
                existing = existing.parent
            require(shutil.disk_usage(existing).free >= MIN_FREE, 'insufficient full comparison/build storage: ' + directory)
        for image in {config['build_image'], lab['build_image']}:
            result = run(['docker', 'image', 'inspect', image], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
            require(result.returncode == 0, 'missing pinned local build image: ' + image)
        require(Path(args.af).is_file() and os.access(args.af, os.X_OK), 'missing native af executable')
        with tempfile.TemporaryDirectory() as temporary:
            env = dict(PATH=os.environ.get('PATH', '/usr/bin:/bin'), HOME=temporary, AF_SELF_OFFLINE='1',
                XDG_CONFIG_HOME=temporary, XDG_STATE_HOME=temporary, XDG_CACHE_HOME=temporary, XDG_DATA_HOME=temporary)
            result = run([args.af, '--version', '--json'], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env, timeout=30)
            require(result.returncode == 0 and json.loads(result.stdout)['commit'] == lab['control_sha'][:12], 'af differs from control_sha')
            result = run([args.af, 'ant', 'validate', '--help'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env, timeout=30)
            require(result.returncode == 0, 'af lacks native Ant support')
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.TimeoutExpired, runtime.ContractError, build.Refused) as error:
        errors.append('deployment prerequisite refused: ' + str(error))
    return errors


def launcher(source, service, pins, mode):
    return ('#!/usr/bin/python3 -IB\nimport hashlib,os,runpy,sys\nsys.dont_write_bytecode=True\n'
            'pins=' + repr(pins) + '\n'
            'for path,digest in pins.items():\n'
            ' with open(path,"rb") as stream: actual="sha256:"+hashlib.sha256(stream.read()).hexdigest()\n'
            ' if actual!=digest: raise SystemExit("Installed nightly authority changed: "+path)\n'
            'sys.argv=[' + repr(str(source)) + ',' + repr(mode) + ',"--service",' + repr(str(service)) + ']\n'
            'runpy.run_path(' + repr(str(source)) + ',run_name="__main__")\n')


def install(args):
    errors = dependency_errors(args)
    require(not errors, 'Missing deployment prerequisites:\n' + '\n'.join(errors))
    import build
    config = build.load_config(args.config)
    output, root = Path(args.output), Path(args.root)
    runtime.private(output.parent, directory=True)
    runtime.private(root.parent, directory=True)
    require(not output.exists() and not output.is_symlink() and not root.exists() and not root.is_symlink(),
            'installation and job root must be absent; never reset receipts')
    service = dict(schema='af/nightly-service@1', root=args.root, repository=config['repository'],
        nightly_config=args.config, nightly_config_digest=sha256_bytes(runtime.read(args.config)),
        executor_config=config['report']['executor_config'],
        executor_config_digest=sha256_bytes(runtime.read(config['report']['executor_config'])),
        max_jobs=60, max_job_wall_ms=LIMITS['max_wall_ms'], max_bytes=2**40)
    runtime.wire.validate(service, 'nightly-service-v1.json')
    output.mkdir(mode=0o700)
    root.mkdir(mode=0o700)
    (root / 'jobs').mkdir(mode=0o700)
    for name in ('queue.lock', 'worker.lock'):
        write_new(root / name, b'')
    service_file = output / 'service.json'
    write_new(service_file, document_bytes(service))
    pins = {str(path): sha256_bytes(path.read_bytes()) for path in [*authority_paths(), service_file]}
    for mode in ('adapter', 'worker'):
        write_new(output / mode, launcher(HERE / 'scenario.py', service_file, pins, mode), executable=True)
    # The native tick driver also refuses a replaced binary before touching the Store.
    native_digest = sha256_bytes(Path(args.af).read_bytes())
    tick = ('#!/usr/bin/python3 -IB\nimport hashlib,os\np=' + repr(args.af) + '\n'
            'with open(p,"rb") as f: d="sha256:"+hashlib.sha256(f.read()).hexdigest()\n'
            'if d!=' + repr(native_digest) + ': raise SystemExit("native af changed")\n'
            'os.execv(p,[p,"ant","tick","--state",' + repr(args.state) + ',"--json"])\n')
    write_new(output / 'tick', tick, executable=True)
    digest = canonical_digest(service)
    definition = bundle(digest, config['repository'])
    policy = dict(schema='af/scenario-policy@8', adapters=dict(nightly=dict(executable=str(output / 'adapter'),
        digest=sha256_bytes((output / 'adapter').read_bytes()), operations=[*runtime.OPS, 'observe'],
        destinations=['github:' + config['repository']], max_wall_ms=30000, max_output_bytes=65536)),
        task_plans=[], task_templates=[], decision_keys={}, file_roots=[], performance_publications={},
        nightly_releases=dict(nightly=dict(repository=config['repository'], configuration_digest=digest)))
    for name, value in (('scenario.json', definition), ('policy.json', policy), ('ant.json', ant(definition, args.start_unix_ms))):
        write_new(output / name, document_bytes(value))
    for name, value in units(args.af, args.repo, args.state, args.output, args.user).items():
        write_new(output / name, value)
    write_new(output / 'review.txt', 'Prepared only; validate Scenario/Ant and review exact pins before registration.\n'
              'af executable digest: ' + native_digest + '\nNo services registered or enabled.\n')
    executor.sync_dir(str(output))
    executor.sync_dir(str(root / 'jobs'))
    executor.sync_dir(str(root))
    executor.sync_dir(str(output.parent))
    executor.sync_dir(str(root.parent))
    print('Prepared pinned inactive installation: ' + args.output)


def prepare(args):
    output = Path(args.output)
    require(not output.exists() and not output.is_symlink(), 'prepare output must be absent')
    output.mkdir(mode=0o700)
    installation = '/var/lib/af-nightly/native'
    for name, value in units(args.af, args.repo, args.state, installation, args.user).items():
        write_new(output / name, value)
    command = ('/usr/bin/python3 -IB ' + args.repo + '/scripts/nightly/install.py install --config ' + args.config
        + ' --af ' + args.af + ' --repo ' + args.repo + ' --root ' + args.root + ' --state ' + args.state
        + ' --output ' + installation + ' --user ' + args.user + ' --start-unix-ms ' + str(args.start_unix_ms))
    errors = dependency_errors(args)
    write_new(output / 'INSTALL.txt', '\n'.join(['Prepared only; no machine activation or release.',
        'Run as the worker account after installing the exact reviewed root-owned control_sha.',
        'Keep same-worker private configs/job store and signing secrets outside the control checkout.',
        'The embedded nightly trust root is the explicitly approved 0596484DC93A8A62 public key.',
        'Prerequisites observed:', *['- ' + item for item in errors], command,
        'Validate pins, register disabled, and prove one explicit nightly tag manually.',
        'Stop/disable af-nightly.timer and all four legacy chain services; mask all five retired units.',
        'Only after manual proof install the new units, enable the Ant and both minute timers.',
        'The pinned Ant owns 03:00 Europe/Moscow; timers only drive ticks and workers.', '']))
    print('Prepared concrete inactive machine files: ' + args.output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('prepare', 'install'))
    for key in ('config', 'af', 'repo', 'root', 'state', 'output', 'user'):
        parser.add_argument('--' + key, required=True)
    parser.add_argument('--start-unix-ms', required=True, type=int)
    args = parser.parse_args()
    os.umask(0o077)
    try:
        for key in ('config', 'af', 'repo', 'root', 'state', 'output'):
            safe_path(getattr(args, key))
        require(re.fullmatch(r'[a-z_][a-z0-9_-]{0,31}', args.user), 'unsafe service account')
        require(0 < args.start_unix_ms <= 8640000000000, 'invalid start')
        (prepare if args.command == 'prepare' else install)(args)
    except (OSError, ValueError, RuntimeError, runtime.ContractError) as error:
        print('Nightly installation refused: ' + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
