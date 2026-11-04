#!/usr/bin/env python3
"""CI disk provisioning tests: fake privilege commands, real filesystem safety checks."""
import contextlib
import importlib.util
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parent.parent
TOOL = ROOT / 'scripts/ci-disk.py'
SPEC = importlib.util.spec_from_file_location('ci_disk', TOOL)
disk = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(disk)


class CiDisk(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.env = dict(GITHUB_ACTIONS='true', RUNNER_OS='Linux',
                        RUNNER_ENVIRONMENT='github-hosted', ImageOS='ubuntu24')
        for name in ('GITHUB_WORKSPACE', 'RUNNER_TEMP', 'HOME', 'RUNNER_TOOL_CACHE'):
            path = self.root / name
            path.mkdir()
            (path / 'keep').write_text('protected')
            self.env[name] = str(path)
        self.sdks = (self.root / 'android', self.root / 'dotnet', self.root / 'swift')
        for path in self.sdks:
            path.mkdir()
            (path / 'payload').write_text('SDK')

    def prepare(self, free=41 * disk.GIB, command=None):
        with (patch.object(disk.platform, 'system', return_value='Linux'),
              patch.object(disk, 'UNUSED_SDKS', self.sdks),
              patch.object(disk.subprocess, 'run', side_effect=command) as run,
              patch.object(disk.shutil, 'disk_usage') as usage,
              contextlib.redirect_stdout(io.StringIO()) as output):
            usage.return_value.free = free
            disk.prepare(self.env)
        return run, output.getvalue()

    def test_only_fixed_unused_sdk_payloads_are_eligible(self):
        self.assertEqual(tuple(map(str, disk.UNUSED_SDKS)), (
            '/usr/local/lib/android', '/usr/share/dotnet', '/usr/local/.ghcup', '/opt/ghc',
            '/usr/share/swift'))

    def test_local_mac_self_hosted_and_unknown_images_never_dispatch_commands(self):
        # Run the actual entrypoint: even a fake sudo must never be reached.
        fakebin = self.root / 'bin'
        fakebin.mkdir()
        marker = self.root / 'called'
        for name in ('sudo', 'df'):
            tool = fakebin / name
            tool.write_text('#!/bin/sh\nprintf called > "$MARKER"\nexit 99\n')
            tool.chmod(0o755)
        cases = ({}, {'RUNNER_OS': 'macOS'}, {'GITHUB_ACTIONS': 'false'},
                 {'RUNNER_ENVIRONMENT': 'self-hosted'}, {'ImageOS': 'unknown'})
        for changes in cases:
            env = dict(self.env, PATH=str(fakebin), MARKER=str(marker))
            if not changes:
                env.pop('GITHUB_ACTIONS')
            env.update(changes)
            result = subprocess.run([sys.executable, '-I', str(TOOL)], env=env,
                                    text=True, capture_output=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('skipped', result.stdout)
            self.assertFalse(marker.exists())
        with patch.object(disk.platform, 'system', return_value='Darwin'):
            self.assertFalse(disk.hosted_linux(self.env))

    def test_hosted_cleanup_has_before_after_diagnostics_and_exact_commands(self):
        run, output = self.prepare()
        self.assertEqual([call.args[0] for call in run.call_args_list], [
            ['df', '-h', '--', '/', self.env['GITHUB_WORKSPACE'], self.env['RUNNER_TEMP']],
            *[['sudo', '-n', 'rm', '-rf', '--one-file-system', '--', str(p)] for p in self.sdks],
            ['df', '-h', '--', '/', self.env['GITHUB_WORKSPACE'], self.env['RUNNER_TEMP']],
        ])
        self.assertTrue(all(call.kwargs == {'check': True} for call in run.call_args_list))
        self.assertIn('before SDK cleanup', output)
        self.assertIn('after SDK cleanup', output)
        self.assertIn('free=41.0 GiB; reclaimed=0.0 GiB', output)
        for name in ('GITHUB_WORKSPACE', 'RUNNER_TEMP', 'HOME', 'RUNNER_TOOL_CACHE'):
            self.assertEqual((Path(self.env[name]) / 'keep').read_text(), 'protected')

    def test_missing_sdk_is_skipped_without_broadening_cleanup(self):
        self.sdks = (self.root / 'absent',)
        run, _ = self.prepare()
        self.assertEqual(len(run.call_args_list), 2)  # Only df, never sudo.

    def test_less_than_build_headroom_plus_production_floor_fails(self):
        with self.assertRaisesRegex(ValueError, '40 GiB free before build'):
            self.prepare(free=40 * disk.GIB - 1)
        self.prepare(free=40 * disk.GIB)

    def test_each_filesystem_must_have_headroom(self):
        with (patch.object(disk, 'diagnostics', return_value={
                Path('/'): 50 * disk.GIB,
                Path(self.env['GITHUB_WORKSPACE']): 50 * disk.GIB,
                Path(self.env['RUNNER_TEMP']): 9 * disk.GIB}),
              self.assertRaisesRegex(ValueError, '40 GiB free before build')):
            self.prepare()

    def test_missing_or_relative_runner_paths_refuse_before_commands(self):
        for name in ('GITHUB_WORKSPACE', 'RUNNER_TEMP', 'HOME', 'RUNNER_TOOL_CACHE'):
            for value in ('', 'relative', str(self.root / 'missing')):
                with self.subTest(name=name, value=value):
                    env = dict(self.env, **{name: value})
                    with self.assertRaisesRegex(ValueError, name):
                        disk.protected_paths(env)

    def test_protected_state_and_its_ancestors_or_descendants_are_refused(self):
        for sdk in self.sdks:
            for protected in (sdk, sdk / 'workspace', sdk.parent):
                with (patch.object(disk, 'UNUSED_SDKS', self.sdks),
                      self.assertRaisesRegex(ValueError, 'overlaps protected')):
                    disk.cleanup_paths([protected])
        # Resolved aliases to the checkout cannot bypass overlap protection.
        alias = self.root / 'workspace-alias'
        alias.symlink_to(self.sdks[1], target_is_directory=True)
        self.env['GITHUB_WORKSPACE'] = str(alias)
        with (patch.object(disk, 'UNUSED_SDKS', self.sdks),
              self.assertRaisesRegex(ValueError, 'overlaps protected')):
            disk.cleanup_paths(disk.protected_paths(self.env))

    def test_symlink_leaf_dangling_link_and_ancestor_refuse_before_any_cleanup(self):
        link = self.root / 'link'
        for target in (self.sdks[0], self.root / 'missing'):
            link.symlink_to(target, target_is_directory=True)
            for path in (link, link / 'child'):
                self.sdks = (self.root / 'android', path)
                with (patch.object(disk.platform, 'system', return_value='Linux'),
                      patch.object(disk, 'UNUSED_SDKS', self.sdks),
                      patch.object(disk.subprocess, 'run') as run,
                      self.assertRaisesRegex(ValueError, 'symlink')):
                    disk.prepare(self.env)
                run.assert_not_called()
            link.unlink()

    def test_mounts_and_non_directory_payloads_are_refused(self):
        with (patch.object(Path, 'is_mount', return_value=True),
              patch.object(disk, 'UNUSED_SDKS', self.sdks),
              self.assertRaisesRegex(ValueError, 'ordinary directory')):
            disk.cleanup_paths([])
        file = self.root / 'file'
        file.write_text('keep')
        with (patch.object(disk, 'UNUSED_SDKS', (file,)),
              self.assertRaisesRegex(ValueError, 'ordinary directory')):
            disk.cleanup_paths([])

    def test_failed_cleanup_is_fatal_and_still_prints_after_diagnostics(self):
        calls = []

        def command(argv, **kwargs):
            calls.append(argv)
            if argv[0] == 'sudo':
                raise subprocess.CalledProcessError(1, argv)

        with self.assertRaises(subprocess.CalledProcessError):
            self.prepare(command=command)
        self.assertEqual([call[0] for call in calls], ['df', 'sudo', 'df'])

    def test_only_rust_validation_jobs_prepare_before_build_or_cache_restore(self):
        workflow = (ROOT / '.github/workflows/validate.yml').read_text()
        jobs = {}
        for line in workflow.splitlines():
            if line.startswith('  ') and not line.startswith('   ') and line.endswith(':'):
                name = line.strip()[:-1]
                jobs[name] = []
            elif jobs:
                jobs[name].append(line)
        prepared = set()
        for name, lines in jobs.items():
            body = '\n'.join(lines)
            if 'scripts/ci-disk.py' in body:
                prepared.add(name)
                self.assertIn("if: runner.os == 'Linux' && runner.environment == 'github-hosted'", body)
                self.assertLess(body.index('scripts/ci-disk.py'), body.index('rustup toolchain install'))
                self.assertLess(body.index('scripts/ci-disk.py'), body.index('./.github/actions/cargo-cache'))
        self.assertEqual(prepared, {'base', 'classes', 'container-probes'})
        self.assertNotIn('ci-disk.py', (ROOT / 'scripts/verify.sh').read_text())


if __name__ == '__main__':
    unittest.main()
