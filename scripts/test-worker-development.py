#!/usr/bin/env python3
"""Run the actual companion renderer and hermetic Rust/Python example programs."""
from pathlib import Path
import json
import os
import shutil
import subprocess
import sys
import tempfile

import unittest

ROOT = Path(__file__).resolve().parent.parent
sys.dont_write_bytecode = True
sys.path.insert(0, str(ROOT / "scripts/performance"))
from perflab._vendor import tomli as tomllib
SCRIPT = ROOT / 'scripts/worker-development.py'
EXAMPLES = ROOT / 'fixtures/worker-development'
DIGEST = 'sha256:' + 'a' * 64


class Development(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.binding = self.root / 'binding.toml'
        self.binding.write_text(f'''schema = "af.worker-development-binding/3"
storage = "tmpfs_volume"
runtime = "/usr/bin/docker"
image = "fixture.invalid/toolchain@{DIGEST}"
checksum_program = "/usr/bin/sha256sum"
probe_shell = "/bin/sh"
[tools.sh]
program = "/bin/sh"
identity = "{DIGEST}"
[tools.cargo]
program = "/usr/local/bin/cargo"
identity = "{DIGEST}"
[tools.rustc]
program = "/usr/local/bin/rustc"
identity = "{DIGEST}"
[tools.cc]
program = "/usr/bin/cc"
identity = "{DIGEST}"
[tools.python3]
program = "/usr/local/bin/python3"
identity = "{DIGEST}"
[cache_limits]
max_bytes = 268435456
max_entries = 16384
max_depth = 32
max_path_bytes = 4096
[limits]
wall_ms = 120000
output_bytes = 65536
cpu_millicores = 2000
memory_bytes = 1073741824
processes = 128
disk_bytes = 268435456
''')

    def render(self, *args):
        return subprocess.run([sys.executable, '-I', str(SCRIPT), '--binding', str(self.binding),
                               *map(str, args)], capture_output=True, text=True, timeout=30)

    def test_real_renderer_pins_both_language_examples_without_overwriting_existing_state(self):
        for language in ('rust', 'python'):
            output = self.root / (language + '.toml')
            args = ('--template', EXAMPLES / language / 'development.template.toml', '--output', output)
            result = self.render(*args)
            self.assertEqual(result.returncode, 0, result.stderr)
            request = tomllib.loads(output.read_text())
            self.assertEqual(request['binding_identity'], result.stdout.strip())
            self.assertEqual(request['tools']['sh'], DIGEST)
            self.assertEqual(request['limits']['disk_bytes'], 268435456)
            if language == 'rust':
                self.assertEqual(request['tools']['cc'], DIGEST)
            self.assertNotIn('@BINDING_ID@', output.read_text())
            before = output.read_bytes()
            self.assertNotEqual(self.render(*args).returncode, 0)
            self.assertEqual(output.read_bytes(), before)

    def test_binding_schema_tagged_images_missing_or_unsupported_disk_limits_and_symlinks_fail_closed(self):
        original = self.binding.read_text()
        for changed in (original.replace('/3"', '/2"', 1),
                        original.replace('@' + DIGEST, ':latest'),
                        original.replace('disk_bytes = 268435456', 'disk_bytes = 1024'),
                        original.replace('disk_bytes = 268435456\n', ''),
                        original.replace('tmpfs_volume', 'host_bind')):
            self.binding.write_text(changed)
            self.assertNotEqual(self.render().returncode, 0)
        self.binding.write_text(original)
        alias = self.root / 'alias'
        alias.symlink_to(self.binding)
        self.binding = alias
        self.assertNotEqual(self.render().returncode, 0)

    def test_cache_authority_changes_the_exact_binding_pin_and_old_versions_fail_closed(self):
        original = self.binding.read_text()
        first = self.render()
        self.assertEqual(first.returncode, 0, first.stderr)
        self.binding.write_text(original + '\n[dependency_sources]\npackages = "' + DIGEST + '"\n')
        approved = self.render()
        self.assertEqual(approved.returncode, 0, approved.stderr)
        self.assertNotEqual(approved.stdout, first.stdout)
        self.binding.write_text(original.replace('max_bytes = 268435456', 'max_bytes = 134217728'))
        limited = self.render()
        self.assertEqual(limited.returncode, 0, limited.stderr)
        self.assertNotEqual(limited.stdout, first.stdout)
        self.binding.write_text(original.replace('/3"', '/2"', 1))
        self.assertNotEqual(self.render().returncode, 0)
        self.binding.write_text(original)
        output = self.root / 'cached.toml'
        result = self.render('--template', EXAMPLES / 'rust/development.template.toml', '--output', output)
        self.assertEqual(result.returncode, 0, result.stderr)
        policy = tomllib.loads(output.read_text())['caches']['objects']
        self.assertEqual(policy['lifecycle'], 'task')
        self.assertEqual(policy['fingerprints'], ['Cargo.lock', 'Cargo.toml'])
        self.assertEqual((policy['input'], policy['output']), ('cache_in', 'cache_out'))

    def test_python_reviewer_harness_compiles_tests_and_keeps_private_state(self):
        source = self.root / 'source'
        shutil.copytree(EXAMPLES / 'python', source)
        state = self.root / 'state'
        state.mkdir()
        build = self.root / 'build'
        build.mkdir()
        env = {'PATH': os.environ.get('PATH', os.defpath), 'HOME': str(state), 'BUILD_ROOT': str(build),
               'PYTHONDONTWRITEBYTECODE': '1'}
        result = subprocess.run(['/bin/sh', str(source / 'harness.sh')], cwd=source, env=env,
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)['schema'], 'af.worker-reply/1')
        self.assertTrue((build / 'fixture.pyc').is_file())
        self.assertFalse((source / '__pycache__').exists())
        self.assertEqual((state / 'first-command').read_text(), 'first command completed\n')

    def rust_tool_bin(self, env):
        # The gate admits real binaries on PATH with an empty private RUSTUP_HOME.
        # Query that compiler, not rustup's installation registry. An ordinary rustup
        # proxy also supports this query, but must never acquire a missing toolchain.
        channel = tomllib.loads((ROOT / 'rust-toolchain.toml').read_text())['toolchain']['channel']
        probe = subprocess.run(['rustc', '--print', 'sysroot'], cwd=ROOT,
                               env={**env, 'RUSTUP_AUTO_INSTALL': '0'},
                               capture_output=True, text=True, timeout=30)
        self.assertEqual(probe.returncode, 0, probe.stderr)
        sysroot = Path(probe.stdout.strip())
        self.assertTrue(sysroot.is_absolute(), probe.stdout)
        tool_bin = sysroot / 'bin'
        for name in ('cargo', 'rustc', 'rustdoc'):
            self.assertTrue((tool_bin / name).is_file(), str(tool_bin / name))
        version = subprocess.run([str(tool_bin / 'rustc'), '--version'], cwd=ROOT,
                                 env={**env, 'RUSTUP_AUTO_INSTALL': '0'},
                                 capture_output=True, text=True, timeout=30)
        self.assertEqual(version.returncode, 0, version.stderr)
        self.assertEqual(version.stdout.split()[:2], ['rustc', channel])
        return tool_bin

    def test_rust_implementer_harness_compiles_tests_twice_in_its_private_environment(self):
        tool_bin = self.rust_tool_bin(os.environ)
        source = self.root / 'rust-source'
        shutil.copytree(EXAMPLES / 'rust', source)
        state = self.root / 'rust-state'
        state.mkdir()
        cache = self.root / 'rust-cache'
        cache.mkdir()
        rustup = self.root / 'rustup'
        rustup.mkdir()
        scratch = self.root / 'rust-scratch'
        scratch.mkdir()
        # Local builds stay in the checkout's target/. verify.sh supplies its owned
        # gate target explicitly because a gate's source Snapshot is read-only.
        # The fixture's child build directory is removed on every exit.
        target = Path(os.environ.get('AF_GATE_NEXTEST_TARGET', ROOT / 'target'))
        target.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='worker-development-fixture-', dir=target) as build:
            env = {'PATH': str(tool_bin) + os.pathsep + os.defpath, 'HOME': str(state),
                   'CARGO_HOME': str(cache), 'RUSTUP_HOME': str(rustup),
                   'RUSTUP_AUTO_INSTALL': '0', 'TMPDIR': str(scratch),
                   'CARGO_TARGET_DIR': build, 'CARGO_NET_OFFLINE': 'true'}
            # Reproduce native gate discovery: real admitted tools, no rustup
            # installation or ambient Cargo cache, and no network acquisition.
            self.assertEqual(self.rust_tool_bin(env), tool_bin)
            result = subprocess.run(['/bin/sh', str(source / 'harness.sh')], cwd=source, env=env,
                                    capture_output=True, text=True, timeout=120)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)['schema'], 'af.worker-reply/1')
            self.assertEqual(result.stderr.count('1 passed'), 2)
            self.assertEqual((state / 'first-command').read_text(), 'first command completed\n')
            self.assertTrue((Path(build) / 'debug').is_dir())
            self.assertEqual(list(rustup.iterdir()), [])


if __name__ == '__main__':
    unittest.main()
