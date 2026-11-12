#!/usr/bin/env python3
"""Offline release preflight regressions, with no binary, benchmark or GitHub access."""
import json
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
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
    # copyfile does not preserve read-only file modes, unlike copy/copy2. copytree
    # still preserves the directory mode; schemas is a flat directory we also mutate.
    shutil.copyfile(source / 'scripts/release-contract.json', destination / 'scripts/release-contract.json')
    for fixture in (source / 'fixtures').glob('telemetry-bench'):
        shutil.copytree(fixture, destination / 'fixtures' / fixture.name, copy_function=shutil.copyfile)
    schemas = destination / 'schemas'
    shutil.copytree(source / 'schemas', schemas, copy_function=shutil.copyfile)
    schemas.chmod(schemas.stat().st_mode | stat.S_IRWXU)


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

    def test_real_preflight_cli_and_schema_writer_refuse_drift(self):
        shutil.copy(ROOT / 'scripts/release-contract.py', self.root / 'scripts')
        lab = self.root / 'scripts/performance'
        lab.mkdir()
        shutil.copytree(ROOT / 'scripts/performance/perflab', lab / 'perflab')
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
