#!/usr/bin/env python3
"""Offline release preflight regressions, with no binary, benchmark or GitHub access."""
import hashlib
import json
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts/performance'))
from perflab import contracts, telemetry_lab  # noqa: E402

PREFLIGHT = telemetry_lab.script_module('scripts/release-contract.py')


def copy_inputs(source, destination):
    """Keep the source seal; only disposable fixtures receive write permission."""
    (destination / 'scripts').mkdir()
    shutil.copyfile(source / 'scripts/telemetry-overhead.py',
                    destination / 'scripts/telemetry-overhead.py')
    helper = Path('scripts/performance/perflab/account_isolation.py')
    (destination / helper).parent.mkdir(parents=True)
    shutil.copyfile(source / helper, destination / helper)
    # copyfile does not preserve read-only file modes, unlike copy/copy2. copytree
    # still preserves the directory mode; schemas is a flat directory we also mutate.
    declaration = json.loads((source / 'scripts/release-contract.json').read_text())
    declaration['version'] = '0.15.1-rc.3'
    (destination / 'scripts/release-contract.json').write_text(json.dumps(declaration))
    for fixture in (source / 'fixtures').glob('telemetry-bench'):
        shutil.copytree(fixture, destination / 'fixtures' / fixture.name, copy_function=shutil.copyfile)
    schemas = destination / 'schemas'
    shutil.copytree(source / 'schemas', schemas, copy_function=shutil.copyfile)
    schemas.chmod(schemas.stat().st_mode | stat.S_IRWXU)


class HostedReleaseDependencies(unittest.TestCase):
    def test_base_installs_real_signature_dependency_before_release_checks(self):
        workflow = (ROOT / '.github/workflows/validate.yml').read_text()
        base = workflow.split('\n  base:\n', 1)[1].split('\n  classes:\n', 1)[0]
        self.assertIn('"ubuntu-latest", "macos-latest"', base)
        install, checks = base.split('      - name: Always-on format, lint, policy and binary smoke', 1)
        self.assertIn('make fmt lint base-check', checks)
        step = install.split('      - name: Install release-check signature verifier\n', 1)[1]
        self.assertIn('        shell: bash\n', step)
        script = '\n'.join(line[10:] for line in step.split('        run: |\n', 1)[1].splitlines())
        # Exercise the actual workflow shell without installing packages or contacting a service.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / 'commands'
            for name in ('sudo', 'brew'):
                stub = root / name
                stub.write_text('#!/bin/bash\nprintf "%s\\n" "${0##*/} $*" >> "$COMMAND_LOG"\n'
                                'exit "$INSTALL_EXIT"\n')
                stub.chmod(0o755)
            for runner, commands in (
                ('Linux', ['sudo apt-get update',
                           'sudo apt-get install -y --no-install-recommends minisign']),
                ('macOS', ['brew install minisign']),
            ):
                for exit_code in (0, 23):
                    with self.subTest(runner=runner, exit_code=exit_code):
                        log.write_text('')
                        result = subprocess.run([shutil.which('bash'), '-c', script],
                            env={'PATH': str(root), 'RUNNER_OS': runner,
                                 'COMMAND_LOG': str(log), 'INSTALL_EXIT': str(exit_code)},
                            capture_output=True, text=True)
                        self.assertEqual(result.returncode, exit_code, result.stderr)
                        self.assertEqual(log.read_text().splitlines(),
                                         commands if exit_code == 0 else commands[:1])
        policy = json.loads((ROOT / 'scripts/release-policy.json').read_text())
        self.assertEqual(policy['pr_validation']['source_files']['.github/workflows/validate.yml'],
                         hashlib.sha256(workflow.strip().encode()).hexdigest())

    def test_release_build_installs_private_account_dependencies_for_each_linux_target(self):
        import re
        workflow = (ROOT / '.github/workflows/release.yml').read_text()
        build = workflow.split('\n  build:\n', 1)[1].split('\n  publish:\n', 1)[0]
        self.assertIn('scripts/release-smoke.sh "$run" "$RUNNER_TEMP/smoke"', build)
        before_build = build.split('      - name: Build\n', 1)[0]
        marker = '      - name: Install Linux smoke dependencies\n'
        self.assertIn(marker, before_build,
                      'Linux release smoke requires its bwrap namespace before binary execution')
        step = before_build.split(marker, 1)[1].split('      - name:', 1)[0]
        self.assertIn("        if: runner.os == 'Linux'\n", step)
        self.assertIn('        shell: bash\n', step)
        self.assertIn('run: bash scripts/prepare-linux-release-smoke.sh', step)
        script = (ROOT / 'scripts/prepare-linux-release-smoke.sh').read_text()
        proof = (ROOT / '.github/workflows/release-namespace-proof.yml').read_text()
        self.assertIn('runs-on: ubuntu-24.04', proof)
        self.assertIn('run: bash scripts/prepare-linux-release-smoke.sh', proof)
        self.assertIn('persist-credentials: false', proof)
        matrix = re.findall(r'- target: (\S+)\n\s+os: (\S+)\n\s+cross: (\S+)', build)
        linux = [(target, cross) for target, runner, cross in matrix if runner.startswith('ubuntu-')]
        self.assertEqual({cross for _, cross in linux}, {'zig', 'zig-qemu'})
        self.assertIn('in_private_account_namespace()', script)
        self.assertIn('assert os.geteuid() != 0', script)
        self.assertIn("python3 -I -B - <<'PY'", script)
        self.assertIn("[sys.executable, '-I', '-B', '-c'", script)
        self.assertNotIn('sysctl', script)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'scripts').mkdir()
            helper = root / 'scripts/prepare-linux-release-smoke.sh'
            helper.write_text(script)
            log = root / 'commands'
            sudo = root / 'sudo'
            sudo.write_text("""#!/bin/bash
printf '%s\\n' "sudo $*" >> "$COMMAND_LOG"
if [[ "$1" == test ]]; then exit "$PROFILE_EXISTS"; fi
if [[ "$1" == "$FAIL_AT" || "$*" == "$FAIL_AT" ]]; then exit 23; fi
""")
            sudo.chmod(0o755)
            python = root / 'python3'
            python.write_text("""#!/bin/bash
printf '%s\\n' "python3 $*" >> "$COMMAND_LOG"
if [[ "$FAIL_AT" == probe ]]; then exit 23; fi
""")
            python.chmod(0o755)
            # dirname is the only filesystem utility used outside the sudo boundary.
            (root / 'dirname').symlink_to(shutil.which('dirname'))
            packaged = '/usr/share/apparmor/extra-profiles/bwrap-userns-restrict'
            active = '/etc/apparmor.d/bwrap-userns-restrict'
            for target, _ in linux:
                for exists in (0, 1):
                    commands = ['sudo apt-get update',
                        'sudo apt-get install -y --no-install-recommends bubblewrap apparmor-profiles',
                        'sudo test -e ' + active,
                        ('sudo cmp -- ' if exists == 0 else 'sudo install -m 0644 -- ') +
                        packaged + ' ' + active,
                        'sudo apparmor_parser -r -- ' + active, 'python3 -I -B -']
                    for fail_at in ('', 'apt-get update', 'apt-get install -y --no-install-recommends bubblewrap apparmor-profiles',
                                    'cmp' if exists == 0 else 'install', 'apparmor_parser', 'probe'):
                        with self.subTest(target=target, exists=exists, fail_at=fail_at):
                            log.write_text('')
                            result = subprocess.run([shutil.which('bash'), str(helper)],
                                env={'PATH': str(root), 'COMMAND_LOG': str(log),
                                     'PROFILE_EXISTS': str(exists), 'FAIL_AT': fail_at},
                                capture_output=True, text=True)
                            self.assertEqual(result.returncode, 23 if fail_at else 0, result.stderr)
                            expected = commands
                            if fail_at:
                                index = next(i for i, command in enumerate(commands)
                                    if (command.startswith('sudo ' + fail_at) if fail_at != 'probe'
                                        else command.startswith('python3 ')))
                                expected = commands[:index + 1]
                            self.assertEqual(log.read_text().splitlines(), expected)



class ReleaseContract(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        copy_inputs(ROOT, self.root)
        self.version('0.15.1-rc.3')

    def test_read_only_source_copies_allow_all_contract_mutations(self):
        # Model the AF gate's sealed source even in a writable developer checkout.
        source = self.root / 'sealed'
        source.mkdir()
        copy_inputs(ROOT, source)
        files = [source / 'scripts/telemetry-overhead.py', *sorted((source / 'schemas').iterdir())]
        original = {path: path.read_bytes() for path in files}
        for path in files:
            path.chmod(0o444)
        (source / 'schemas').chmod(0o555)
        self.addCleanup((source / 'schemas').chmod, 0o755)
        destination = self.root / 'mutable'
        destination.mkdir()
        copy_inputs(source, destination)
        script = destination / 'scripts/telemetry-overhead.py'
        declaration = destination / 'scripts/release-contract.json'
        document = json.loads(declaration.read_text())
        document['version'] = '9.0.0'
        declaration.write_text(json.dumps(document))
        (destination / 'Cargo.toml').write_text('[workspace.package]\nversion = "9.0.0"\n')
        self.assertEqual(PREFLIGHT.check(destination), '9.0.0')
        schema = destination / 'schemas' / next(iter(contracts.schema_documents()))
        schema.write_text('{}')
        with self.assertRaisesRegex(ValueError, 'stale'):
            PREFLIGHT.check(destination)
        schema.unlink()
        with self.assertRaisesRegex(ValueError, 'missing'):
            PREFLIGHT.check(destination)
        schema.write_bytes(original[source / 'schemas' / schema.name])
        (schema.parent / 'performance-retired-v0.json').write_text('{}')
        with self.assertRaisesRegex(ValueError, 'retired or unowned'):
            PREFLIGHT.check(destination)
        for path, data in original.items():
            self.assertEqual(path.read_bytes(), data)
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o444)
        self.assertEqual(stat.S_IMODE((source / 'schemas').stat().st_mode), 0o555)

    def version(self, version):
        (self.root / 'Cargo.toml').write_text('[workspace.package]\nversion = "%s"\n' % version)

    def test_explicit_compatible_version_passes(self):
        self.assertEqual(PREFLIGHT.check(self.root), '0.15.1-rc.3')
        for version in ('0.15.1-rc.4', '9.0.0'):
            self.version(version)
            PREFLIGHT.prepare(self.root, 'a' * 40)
            self.assertEqual(PREFLIGHT.check(self.root), version)
            declaration = json.loads((self.root / 'scripts/release-contract.json').read_text())
            self.assertEqual(declaration['source'], 'a' * 40)

    def test_current_scenario_contracts_are_declared_and_drift_is_refused(self):
        policy = json.loads((ROOT / 'scripts/release-policy.json').read_text())
        # Independent expectations: do not infer the current generation from either the
        # declaration or the schemas being checked. Both can drift together.
        expected = {'scenario-store': 17, 'scenario-driver': 2,
                    'scenario-adapter-response': 3, 'github-adapter-state': 9,
                    'git-candidate-source-request': 2, 'git-candidate-source': 2, 'scenario': 10, 'scenario-bundle': 10,
                    'scenario-pin': 11, 'scenario-run': 11, 'scenario-list': 11,
                    'ant': 11, 'ant-pin': 12, 'ant-tick': 12, 'ant-state': 13,
                    'ant-inspection': 8, 'ant-list': 13,
                    'scenario-policy': 8, 'implementation-startup-scope': 2, 'implementation-startup-failure': 1,
                    'scenario-task-template': 2, 'scenario-template-pin': 2,
                    'scenario-artifact': 1}
        for name, version in expected.items():
            relative = 'schemas/%s-v%s.json' % (name, version)
            with self.subTest(schema=relative):
                self.assertIn('af/%s@%s' % (name, version), policy['required_features'][relative])
                path = self.root / relative
                original = path.read_bytes()
                document = json.loads(original)
                self.assertEqual(document['properties']['schema']['const'], 'af/%s@%s' % (name, version))
                for invalid in ('af/foreign@1', 'af/%s@%s' % (name, version - 1)):
                    document['properties']['schema']['const'] = invalid
                    path.write_text(json.dumps(document))
                    with self.assertRaisesRegex(ValueError, 'capabilities differ'):
                        PREFLIGHT.check(self.root)
                    path.write_bytes(original)
                retired = self.root / ('schemas/%s-v%s.json' % (name, version - 1))
                self.assertFalse(retired.exists(), 'retired schema must not remain published')
                # Adding the old filename also changes release capabilities, even if its
                # bytes were copied from the current schema instead of using an old writer.
                retired.write_bytes(original)
                with self.assertRaisesRegex(ValueError, 'capabilities differ'):
                    PREFLIGHT.check(self.root)
                retired.unlink()
        template = json.loads((self.root / 'schemas/scenario-task-template-v2.json').read_text())
        self.assertIn('authority_id', template['required'])
        self.assertIn('bindings', template['required'])
        self.assertEqual(PREFLIGHT.check(self.root), '0.15.1-rc.3')

    def test_combined_wake_and_long_flow_contracts_are_bound_by_one_declaration(self):
        # Neither merge parent's capability pin covers the combined schema closure.
        policy = json.loads((ROOT / 'scripts/release-policy.json').read_text())
        expected = {
            'task-wake-route-v1.json': 'af/task-wake-route@1',
            'task-wake-status-v2.json': 'af/task-wake-status@2',
            'scenario-v10.json': 'af/scenario@10',
            'scenario-store-v17.json': 'af/scenario-store@17',
            'scenario-driver-v2.json': 'af/scenario-driver@2',
            'implementation-startup-scope-v2.json': 'af/implementation-startup-scope@2',
            'performance-executor-v5.json': 'af.perf-executor/5',
            'performance-history-v2.json': 'af.perf-history/2',
            'performance-equivalence-v2.json': 'af.perf-equivalence/2',
            'performance-schedule-v3.json': 'af.perf-schedule/3',
            'performance-run-v8.json': 'af.perf-run/8',
            'performance-supervisor-request-v5.json': 'af.perf-supervisor-request/5',
            'performance-supervisor-result-v6.json': 'af.perf-supervisor-result/6',
        }
        self.assertEqual(PREFLIGHT.check(self.root), '0.15.1-rc.3')
        for name, identity in expected.items():
            path = self.root / 'schemas' / name
            original = path.read_bytes()
            document = json.loads(original)
            with self.subTest(schema=name):
                self.assertEqual(document['properties']['schema']['const'], identity)
                self.assertFalse(document['additionalProperties'])
                if name in ('performance-executor-v5.json', 'performance-history-v2.json',
                            'performance-equivalence-v2.json', 'performance-schedule-v3.json'):
                    self.assertIn(identity, policy['required_features']['schemas/' + name])
                # Even a semantically harmless byte change must invalidate the pin.
                path.write_bytes(original + b'\n')
                with self.assertRaisesRegex(ValueError, 'stale|capabilities differ'):
                    PREFLIGHT.check(self.root)
            path.write_bytes(original)
        self.assertEqual(PREFLIGHT.check(self.root), '0.15.1-rc.3')

    def test_preparation_refuses_changed_capabilities(self):
        self.version('0.15.1-rc.4')
        path = self.root / 'scripts/release-contract.json'
        original = path.read_bytes()
        fixture = next((self.root / 'fixtures').glob('telemetry-bench/*/repo/bench.json'))
        fixture.write_text('{}')
        with self.assertRaisesRegex(ValueError, 'capabilities differ'):
            PREFLIGHT.prepare(self.root, 'a' * 40)
        self.assertEqual(path.read_bytes(), original)

    def test_undeclared_and_incompatible_versions_fail(self):
        for version in ('9.0.0', '0.13.0', '0.11.9', '0.0.0', '0.14.0',
                        '0.14.0-rc.8', '0.15.1', '0.15.1-rc.4',
                        '0.15.1-rc.1+unknown', '0.15.1-rc.2+unknown', '0.15.1-rc.20',
                        '0.15.1-rc.3+unknown', '0.15.1-rc.30',
                        '0.14.0-nightly.20261102', '0.14.0-rc.5+unknown', '0.14.0-rc.7+unknown'):
            self.version(version)
            with self.assertRaises(ValueError):
                PREFLIGHT.check(self.root)

    def test_declared_release_cannot_select_a_retired_workload_or_receipt(self):
        path = self.root / 'scripts/telemetry-overhead.py'
        current = path.read_text()
        for old, retired in (("CURRENT_CONTRACT = 'native-task-review@1'",
                              "CURRENT_CONTRACT = 'check-receipt@2'"),
                             ("CHECK_RECEIPT_CURRENT = 'af/TaskCheckReceipt@2'",
                              "CHECK_RECEIPT_CURRENT = 'af/TaskCheckReceipt@1'")):
            with self.subTest(retired=retired):
                path.write_text(current.replace(old, retired))
                with self.assertRaisesRegex(ValueError, 'current committed fixtures'):
                    PREFLIGHT.check(self.root)

    def test_stale_missing_and_retired_lab_schemas_fail(self):
        name = next(iter(contracts.schema_documents()))
        path = self.root / 'schemas' / name
        original = path.read_bytes()
        path.write_text('{}')
        with self.assertRaisesRegex(ValueError, 'stale'):
            PREFLIGHT.check(self.root)
        path.unlink()
        with self.assertRaisesRegex(ValueError, 'missing'):
            PREFLIGHT.check(self.root)
        path.write_bytes(original)
        (path.parent / 'performance-retired-v0.json').write_text('{}')
        with self.assertRaisesRegex(ValueError, 'retired or unowned'):
            PREFLIGHT.check(self.root)

    def test_only_the_four_rust_owned_schemas_are_excluded(self):
        self.assertEqual(len(contracts.RUST_OWNED_SCHEMAS), 4)
        for name in contracts.RUST_OWNED_SCHEMAS:
            self.assertTrue((self.root / 'schemas' / name).is_file())
            self.assertNotIn(name, contracts.lab_schema_names(self.root / 'schemas'))
        PREFLIGHT.check(self.root)

    def test_missing_smoke_artifact_names_captured_stderr(self):
        smoke = telemetry_lab.script_module('scripts/performance/tests/telemetry_runtime_smoke.py')
        with self.assertRaisesRegex(AssertionError, 'named by no fixture contract'):
            from perflab.telemetry_exec import Outcome
            smoke.failed_task_document(self.root,
                Outcome(1, b'', 0, b'0.99.0 is named by no fixture contract', 52, 0))

    def test_disk_refusal_smoke_requires_incomplete_and_preserves_guards(self):
        smoke = telemetry_lab.script_module('scripts/performance/tests/telemetry_runtime_smoke.py')
        registry, _ = telemetry_lab.load_registry()
        config = telemetry_lab.configuration(registry, 'pr', rounds=1, pairs=1,
                                             scale='smoke', corpus_runs=10)
        document = {
            'phase': {'kind': 'finished'},
            'result': {'execution': 'incomplete', 'acceptance': 'inconclusive'},
            'attempts': 1,
            'run_reports': [{'report': {'nodes': [
                {'node': 'root.nodes.check', 'outcome': {'kind': 'completed'}}]}}],
            'runtime_observations': [],
        }
        diagnostic = b'insufficient_disk: finished/gate_failed/preflight_failed'
        for case in ('valid', 'exhausted', 'completed', 'accepted', 'attempts', 'phase',
                     'check', 'evidence', 'runtime', 'disk', 'cause', 'cleanup'):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                receipt = json.loads(json.dumps(document))
                stderr = diagnostic
                if case in ('exhausted', 'completed'):
                    receipt['result']['execution'] = case
                elif case == 'accepted':
                    receipt['result']['acceptance'] = 'satisfied'
                elif case == 'attempts':
                    receipt['attempts'] = 2
                elif case == 'phase':
                    receipt['phase']['kind'] = 'running'
                elif case == 'check':
                    receipt['run_reports'][0]['report']['nodes'][0]['outcome']['kind'] = 'failed'
                elif case == 'evidence':
                    receipt['run_reports'][0]['report']['error'] = 'Task runtime evidence cannot be empty'
                elif case == 'runtime':
                    receipt['runtime_observations'] = [{}]
                elif case == 'disk':
                    stderr = b'finished/gate_failed/preflight_failed'
                elif case == 'cause':
                    stderr = b'insufficient_disk:'

                calls = []
                def run(execution):
                    calls.append(execution)
                    path = Path(execution.cwd) / 'bench/telemetry-bench-concurrent/00-off'
                    path.mkdir(parents=True)
                    (path / 'stdout').write_text(json.dumps(receipt))
                    (path / 'stderr').write_bytes(stderr)
                    return smoke.telemetry_exec.Outcome(
                        code=1, cleanup='uncertain' if case == 'cleanup' else 'confirmed')

                own = Path(directory) / 'execution'
                executor = SimpleNamespace(directory=str(own), python=sys.executable, run=run)
                if case == 'valid':
                    smoke.exercise(executor, registry, config, '/unused/af', refuse=True)
                    self.assertFalse(own.exists())
                else:
                    with self.assertRaises(AssertionError):
                        smoke.exercise(executor, registry, config, '/unused/af', refuse=True)
                    self.assertTrue(own.exists(), 'failed evidence must be retained')
                self.assertEqual(len(calls), 1)
                self.assertIn('--keep', calls[0].argv)

    def test_real_preflight_cli_and_schema_writer_refuse_drift(self):
        shutil.copy(ROOT / 'scripts/release-contract.py', self.root / 'scripts')
        lab = self.root / 'scripts/performance'
        lab.mkdir(exist_ok=True)
        shutil.copytree(ROOT / 'scripts/performance/perflab', lab / 'perflab', dirs_exist_ok=True)
        shutil.copy(ROOT / 'scripts/performance/schemas.py', lab)
        def run(script, *args):
            return subprocess.run([sys.executable, '-I', str(self.root / script), *args],
                                  capture_output=True, text=True, cwd=self.root)
        result = run('scripts/release-contract.py')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.version('0.15.1-rc.3')
        result = run('scripts/release-contract.py')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('0.15.1-rc.3 explicitly selects current telemetry fixtures', result.stdout)
        self.version('0.15.1-rc.4')
        result = run('scripts/release-contract.py')
        self.assertEqual(result.returncode, 1)
        self.assertIn('named by no fixture contract', result.stderr)
        self.version('9.0.0')
        result = run('scripts/release-contract.py')
        self.assertEqual(result.returncode, 1)
        self.assertIn('named by no fixture contract', result.stderr)
        retired = self.root / 'schemas/performance-retired-v0.json'
        retired.write_text('{}')
        result = run('scripts/performance/schemas.py', '--write')
        self.assertEqual(result.returncode, 1)
        self.assertIn('retired or unowned', result.stderr)
        self.assertTrue(retired.exists(), 'retired schemas require explicit removal')
        for name in contracts.RUST_OWNED_SCHEMAS:
            self.assertEqual((self.root / 'schemas' / name).read_bytes(),
                             (ROOT / 'schemas' / name).read_bytes())

    def test_always_run_and_release_preparation_wiring(self):
        make = (ROOT / 'Makefile').read_text()
        recipe = make.split('release-check:\n', 1)[1].split('\n\n', 1)[0]
        self.assertIn('scripts/release-contract.py', recipe)
        self.assertIn('base-check: release-check', make)
        validate = (ROOT / '.github/workflows/validate.yml').read_text()
        self.assertIn('make fmt lint base-check', validate)
        release = (ROOT / 'scripts/release.sh').read_text()
        self.assertIn('exec "$root/dev" release rc', release)
        controller = (ROOT / 'scripts/dev-release.py').read_text()
        self.assertIn("'scripts/release-contract.py', '--prepare'", controller)
        self.assertIn('harness-digest', controller)



if __name__ == '__main__':
    unittest.main()
