#!/usr/bin/env python3
"""Exercise scripts/markdownlint-tool.py (ADR-0145).

`Synthetic` runs everywhere, offline: a two-package closure served from a local mirror and a
stand-in `node` that records what it was given. `RealTool` runs only with AF_MARKDOWNLINT_E2E=1:
it installs the committed closure from registry.npmjs.org once (or reuses the verified tree in
AF_MARKDOWNLINT_PREFIX), then lints fixtures through `run` offline under a private HOME; a true
network namespace needs non-interactive `sudo`, `unshare` and `setpriv`, or a command prefix in
AF_MARKDOWNLINT_OFFLINE that runs its arguments without a network as this user.
"""
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
import base64
import hashlib
import importlib.util
import io
import json
import os
import re
import shlex
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest

sys.dont_write_bytecode = True
SCRIPTS = Path(__file__).resolve().parent
REPOSITORY = SCRIPTS.parent
TOOL = SCRIPTS / 'markdownlint-tool.py'
SPEC = importlib.util.spec_from_file_location('markdownlint_tool', TOOL)
tool = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tool)
REGISTRY = 'https://registry.npmjs.org/'
PLACEHOLDER = 'sha256:' + '0' * 64

# The stand-in `node`: records its arguments, working directory and environment, then exits
# with FAKE_NODE_EXIT, so a test sees exactly what `run` handed markdownlint-cli2.
FAKE_NODE = r'''#!{python}
import json, os, sys
with open(os.environ["FAKE_NODE_RECORD"], "w") as record:
    json.dump({"argv": sys.argv[1:], "cwd": os.getcwd(), "env": dict(os.environ)}, record)
sys.exit(int(os.environ.get("FAKE_NODE_EXIT", "0")))
'''


def tarball(files, extra=()):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode='w:gz', format=tarfile.PAX_FORMAT) as archive:
        for name, data in sorted(files.items()):
            info = tarfile.TarInfo('package/' + name)
            info.size, info.mode = len(data), 0o644
            archive.addfile(info, io.BytesIO(data))
        for info in extra:
            archive.addfile(info)
    return output.getvalue()


def integrity(data):
    return 'sha512-' + base64.b64encode(hashlib.sha512(data).digest()).decode()


def package(name, version, extra_files=None, **fields):
    files = {'package.json': json.dumps({'name': name, 'version': version, **fields}).encode()}
    files.update(extra_files or {})
    return files


def entry(name, version, data, **fields):
    tail = name.rsplit('/', 1)[-1]
    return {'version': version, 'resolved': f'{REGISTRY}{name}/-/{tail}-{version}.tgz',
            'integrity': integrity(data), 'license': 'MIT', **fields}


def unseal(path):
    for directory, _, _ in os.walk(path):
        os.chmod(directory, 0o700)


class Synthetic(unittest.TestCase):
    """A closure of markdownlint-cli2 0.22.1 and its one dependency, built here."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.cleanup)
        self.root = Path(os.path.realpath(self.tmp.name))
        self.scripts = self.root / 'repo/scripts'
        (self.scripts / 'markdownlint').mkdir(parents=True)
        shutil.copy(TOOL, self.scripts / 'markdownlint-tool.py')
        self.mirror = self.root / 'mirror'
        self.prefix = self.root / 'prefix'
        self.fakebin = self.root / 'fakebin'
        self.fakebin.mkdir()
        (self.fakebin / 'node').write_text(FAKE_NODE.replace('{python}', sys.executable))
        (self.fakebin / 'node').chmod(0o755)
        self.home = self.root / 'home'
        self.home.mkdir()
        self.work = self.root / 'work'
        self.work.mkdir()
        self.record = self.root / 'record.json'
        self.tarballs = {
            'markdownlint-cli2': tarball(package(
                'markdownlint-cli2', '0.22.1',
                {'markdownlint-cli2-bin.mjs': b'// bin\n', 'lib/a.mjs': b'export {};\n'},
                bin={'markdownlint-cli2': 'markdownlint-cli2-bin.mjs'})),
            'markdownlint': tarball(package('markdownlint', '0.40.0', {'lib/b.mjs': b'b\n'})),
        }
        self.lock = {'name': 'fixture', 'version': '0.0.0', 'lockfileVersion': 3,
                     'requires': True, 'packages': {
                         '': {'name': 'fixture', 'version': '0.0.0',
                              'dependencies': {'markdownlint-cli2': '0.22.1'}},
                         'node_modules/markdownlint-cli2': entry(
                             'markdownlint-cli2', '0.22.1', self.tarballs['markdownlint-cli2'],
                             dependencies={'markdownlint': '0.40.0'},
                             bin={'markdownlint-cli2': 'markdownlint-cli2-bin.mjs'}),
                         'node_modules/markdownlint': entry(
                             'markdownlint', '0.40.0', self.tarballs['markdownlint'])}}
        self.publish()
        self.write_pin(PLACEHOLDER)

    def cleanup(self):
        unseal(self.root)
        self.tmp.cleanup()

    def publish(self, name=None, version=None, data=None):
        for key, value in self.lock['packages'].items():
            if key:
                tail = value['resolved'][len(REGISTRY):]
                target = self.mirror / tail
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(self.tarballs.get(tool.package_name(key), b''))
        if name:
            tail = name.rsplit('/', 1)[-1]
            target = self.mirror / name / '-' / f'{tail}-{version}.tgz'
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)

    def write_pin(self, tree):
        pin = self.scripts / 'markdownlint'
        (pin / 'package.json').write_text(json.dumps(
            {'name': 'fixture', 'private': True, 'dependencies': {'markdownlint-cli2': '0.22.1'}}))
        (pin / 'package-lock.json').write_text(json.dumps(self.lock, indent=2))
        (pin / 'tree.sha256').write_text(tree + '\n')

    def tool(self, *args, env=None, cwd=None):
        environment = {'PATH': f'{self.fakebin}:/usr/bin:/bin', 'HOME': str(self.home),
                       'LC_ALL': 'C', 'FAKE_NODE_RECORD': str(self.record)}
        environment.update(env or {})
        return subprocess.run([sys.executable, '-B', str(self.scripts / 'markdownlint-tool.py'),
                               *args], cwd=cwd or self.root, env=environment,
                              capture_output=True, text=True)

    def install(self, *extra):
        return self.tool('install', '--prefix', str(self.prefix), '--from', str(self.mirror),
                         *extra)

    tree = None

    def pinned_tree(self):
        """Install against the placeholder pin; the refusal names the tree it would have placed."""
        first = self.install()
        self.assertEqual(first.returncode, 2, first.stderr)
        self.assertIn('scripts/markdownlint/tree.sha256 pins ' + PLACEHOLDER, first.stderr)
        self.assertEqual(os.listdir(self.prefix), [], 'a refused install leaves nothing')
        return re.search(r'installed tree is (sha256:[0-9a-f]{64})', first.stderr).group(1)

    def installed(self):
        """Pin the fixture closure's tree (the same in every test) and install it."""
        if Synthetic.tree is None:
            Synthetic.tree = self.pinned_tree()
        tree = Synthetic.tree
        self.write_pin(tree)
        second = self.install()
        self.assertEqual(second.returncode, 0, second.stderr)
        root = self.prefix / f'markdownlint-cli2-0.22.1-{tree[7:23]}'
        self.assertIn(f'put on PATH: {root}/bin', second.stdout)
        return root

    def run_tool(self, root, exit_code=0, path=None, env=None):
        if self.record.exists():
            self.record.unlink()
        environment = {'PATH': path or f'{root}/bin:{self.fakebin}:/usr/bin:/bin',
                       'FAKE_NODE_EXIT': str(exit_code), 'NODE_OPTIONS': '--require=/evil.js',
                       'npm_config_registry': 'http://127.0.0.1:9/'}
        environment.update(env or {})
        return self.tool('run', '**/*.md', env=environment, cwd=self.work)

    def recorded(self):
        return json.loads(self.record.read_text()) if self.record.exists() else None

    def assert_refused(self, result, message):
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn('markdownlint-tool: refused:', result.stderr)
        self.assertIn(message, result.stderr)
        self.assertIsNone(self.recorded(), 'markdownlint-cli2 must not start')

    # -- run ----------------------------------------------------------------------------------

    def test_run_hands_markdownlint_its_arguments_directory_and_exit_status(self):
        root = self.installed()
        for status in (0, 1, 2):
            result = self.run_tool(root, status)
            self.assertEqual(result.returncode, status, result.stderr)
            record = self.recorded()
            self.assertEqual(record['argv'], [
                f'{root}/node_modules/markdownlint-cli2/markdownlint-cli2-bin.mjs', '**/*.md'])
            self.assertEqual(os.path.realpath(record['cwd']), str(self.work))
            self.assertEqual(record['env']['HOME'], str(self.home))
            self.assertNotIn('NODE_OPTIONS', record['env'])
            self.assertNotIn('npm_config_registry', record['env'])
        self.assertEqual(os.listdir(self.home), [], 'run writes nothing to HOME')

    def test_a_leading_double_dash_is_not_passed_on(self):
        root = self.installed()
        environment = {'PATH': f'{root}/bin:{self.fakebin}:/usr/bin:/bin'}
        result = self.tool('run', '--', '**/*.md', env=environment, cwd=self.work)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.recorded()['argv'][1:], ['**/*.md'])

    def test_a_missing_tool_fails_closed_without_a_fallback(self):
        self.installed()
        self.assert_refused(self.run_tool(None, path=f'{self.fakebin}:/usr/bin:/bin'),
                            'no markdownlint-cli2-0.22.1-')

    def test_a_missing_node_fails_closed(self):
        root = self.installed()
        self.assert_refused(self.run_tool(root, path=f'{root}/bin'), 'node is not on PATH')

    def test_a_changed_file_is_refused(self):
        root = self.installed()
        target = root / 'node_modules/markdownlint/lib/b.mjs'
        target.chmod(0o644)
        target.write_text('tampered\n')
        target.chmod(0o444)
        self.assert_refused(self.run_tool(root), 'node_modules/markdownlint/lib/b.mjs')

    def test_a_writable_file_is_refused(self):
        root = self.installed()
        (root / 'node_modules/markdownlint/package.json').chmod(0o644)
        self.assert_refused(self.run_tool(root), 'has mode 644, not 444')

    def test_a_missing_file_is_refused(self):
        root = self.installed()
        target = root / 'node_modules/markdownlint/lib'
        target.chmod(0o755)
        (target / 'b.mjs').unlink()
        target.chmod(0o555)
        self.assert_refused(self.run_tool(root), 'node_modules/markdownlint/lib/b.mjs')

    def test_a_link_inside_the_tree_is_refused(self):
        root = self.installed()
        directory = root / 'node_modules/markdownlint'
        directory.chmod(0o755)
        (directory / 'link.mjs').symlink_to('/etc/hostname')
        directory.chmod(0o555)
        self.assert_refused(self.run_tool(root), 'neither a directory nor a regular file')

    def test_a_hard_link_is_refused(self):
        root = self.installed()
        os.link(root / 'node_modules/markdownlint/lib/b.mjs', self.root / 'outside')
        self.assert_refused(self.run_tool(root), 'has another hard link')

    def test_a_manifest_for_another_pin_is_refused(self):
        root = self.installed()
        manifest = root / 'af-tool.json'
        value = json.loads(manifest.read_text())
        value['lock_sha256'] = PLACEHOLDER
        root.chmod(0o755)
        manifest.chmod(0o644)
        manifest.write_text(json.dumps(value))
        manifest.chmod(0o444)
        root.chmod(0o555)
        self.assert_refused(self.run_tool(root), 'names another pin')

    def test_a_prefix_others_can_write_is_refused(self):
        root = self.installed()
        self.prefix.chmod(0o777)
        self.assert_refused(self.run_tool(root), 'writable by other users')
        self.prefix.chmod(0o1777)
        self.assertEqual(self.run_tool(root).returncode, 0, 'a sticky directory is safe')

    def test_a_root_reached_through_a_link_is_verified_where_it_lives(self):
        root = self.installed()
        alias = self.root / 'alias'
        alias.symlink_to(self.prefix)
        result = self.run_tool(root, 1, path=f'{alias / root.name}/bin:{self.fakebin}:/usr/bin')
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(self.recorded()['argv'][0], f'{root}/{tool.BIN}')

    def test_another_pins_tree_is_skipped_and_this_pins_corrupt_tree_is_not(self):
        root = self.installed()
        other = self.prefix / 'markdownlint-cli2-0.22.1-ffffffffffffffff'
        shutil.copytree(root, other, symlinks=True)
        path = f'{other}/bin:{root}/bin:{self.fakebin}:/usr/bin:/bin'
        result = self.run_tool(root, 1, path=path)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertTrue(self.recorded()['argv'][0].startswith(str(root) + '/'))
        # A clean copy of the same pin later on PATH does not stand in for a refused one.
        clean = self.root / 'clean'
        clean.mkdir(mode=0o755)
        shutil.copytree(root, clean / root.name, symlinks=True)
        (root / 'node_modules/markdownlint/package.json').chmod(0o644)
        self.assert_refused(self.run_tool(root, path=f'{root}/bin:{clean / root.name}/bin:'
                                                     f'{self.fakebin}:/usr/bin'), 'has mode 644')
        self.assertEqual(self.run_tool(root, path=f'{clean / root.name}/bin:{self.fakebin}:'
                                                  '/usr/bin').returncode, 0)

    def test_parallel_runs_share_one_read_only_tree(self):
        root = self.installed()

        def one(index):
            record = self.root / f'record-{index}.json'
            work = self.root / f'work-{index}'
            work.mkdir()
            environment = {'PATH': f'{root}/bin:{self.fakebin}:/usr/bin:/bin',
                           'FAKE_NODE_RECORD': str(record), 'FAKE_NODE_EXIT': str(index % 2)}
            result = self.tool('run', '**/*.md', env=environment, cwd=work)
            return result.returncode, os.path.realpath(json.loads(record.read_text())['cwd'])

        with ThreadPoolExecutor(8) as pool:
            results = list(pool.map(one, range(8)))
        self.assertEqual(results, [(i % 2, str(self.root / f'work-{i}')) for i in range(8)])
        self.assertEqual(self.tool('verify', '--prefix', str(self.prefix)).returncode, 0)

    # -- install ------------------------------------------------------------------------------

    def test_a_tree_other_than_the_pinned_one_is_not_placed(self):
        self.assertRegex(self.pinned_tree(), r'^sha256:[0-9a-f]{64}$')

    def test_install_places_an_owned_read_only_tree_and_is_idempotent(self):
        root = self.installed()
        for directory, names, files in os.walk(root):
            info = os.lstat(directory)
            self.assertEqual((info.st_uid, stat.S_IMODE(info.st_mode)), (os.getuid(), 0o555))
            for name in files:
                mode = stat.S_IMODE(os.lstat(Path(directory) / name).st_mode)
                self.assertEqual(mode, 0o555 if name == 'markdownlint-cli2' and
                                 Path(directory).name == 'bin' else 0o444)
        self.assertEqual(sorted(os.listdir(self.prefix)), [root.name])
        shutil.rmtree(self.mirror)
        again = self.install()
        self.assertEqual(again.returncode, 0, 'a verified tree is kept without fetching')

    def test_install_refuses_a_corrupt_tree_unless_asked_to_replace_it(self):
        root = self.installed()
        (root / 'node_modules/markdownlint/package.json').chmod(0o644)
        self.assert_refused(self.install(), 'pass --replace')
        self.assertEqual(self.install('--replace').returncode, 0)
        self.assertEqual(self.run_tool(root).returncode, 0)

    def test_concurrent_installs_place_one_tree(self):
        root = self.installed()
        unseal(self.prefix)
        shutil.rmtree(self.prefix)
        with ThreadPoolExecutor(4) as pool:
            results = list(pool.map(lambda _: self.install().returncode, range(4)))
        self.assertEqual(results, [0, 0, 0, 0])
        self.assertEqual(os.listdir(self.prefix), [root.name], 'no staging directory is left')
        self.assertEqual(self.run_tool(root).returncode, 0)

    def test_a_tarball_that_differs_from_the_lock_is_refused(self):
        self.tarballs['markdownlint'] = tarball(package('markdownlint', '0.40.0'))
        self.publish()
        self.assert_refused(self.install(), "does not match the lock's integrity")
        self.assertEqual(os.listdir(self.prefix), [])

    def test_a_tarball_holding_another_package_is_refused(self):
        data = tarball(package('markdownlint', '0.39.0', {'lib/b.mjs': b'b\n'}))
        self.tarballs['markdownlint'] = data
        self.lock['packages']['node_modules/markdownlint']['integrity'] = integrity(data)
        self.publish()
        self.write_pin(PLACEHOLDER)
        self.assert_refused(self.install(), 'the tarball holds markdownlint 0.39.0')

    def test_link_and_traversal_entries_in_a_tarball_are_refused(self):
        link = tarfile.TarInfo('package/lib/link.mjs')
        link.type, link.linkname = tarfile.SYMTYPE, '/etc/passwd'
        escape = tarfile.TarInfo('package/../../escape')
        for member, message in ((link, 'link or special file'), (escape, 'unsafe path')):
            data = tarball(package('markdownlint', '0.40.0'), [member])
            self.tarballs['markdownlint'] = data
            self.lock['packages']['node_modules/markdownlint']['integrity'] = integrity(data)
            self.publish()
            self.write_pin(PLACEHOLDER)
            self.assert_refused(self.install(), message)
            self.assertFalse((self.root / 'escape').exists())
            self.assertEqual(os.listdir(self.prefix), [])

    def test_locks_that_are_not_an_exact_registry_closure_are_refused(self):
        cases = [
            ('unreachable', lambda p: p.update({'node_modules/extra': entry(
                'extra', '1.0.0', b'x')}), 'nothing depends on'),
            ('missing', lambda p: p.pop('node_modules/markdownlint'), 'which the lock lacks'),
            ('mirror', lambda p: p['node_modules/markdownlint'].update(
                resolved='https://example.invalid/markdownlint-0.40.0.tgz'),
             'does not resolve to its registry tarball'),
            ('script', lambda p: p['node_modules/markdownlint'].update(hasInstallScript=True),
             'unsupported fields'),
            ('link', lambda p: p['node_modules/markdownlint'].update(link=True),
             'unsupported fields'),
            ('range', lambda p: p['node_modules/markdownlint'].update(version='^0.40.0'),
             'no exact version'),
            ('sha1', lambda p: p['node_modules/markdownlint'].update(integrity='sha1-AAAA'),
             'no single sha512 integrity'),
            ('version', lambda p: p['node_modules/markdownlint-cli2'].update(version='0.22.0'),
             'does not resolve to its registry tarball'),
        ]
        original = json.loads(json.dumps(self.lock))
        for name, mutate, message in cases:
            with self.subTest(name):
                self.lock = json.loads(json.dumps(original))
                mutate(self.lock['packages'])
                self.write_pin(PLACEHOLDER)
                with self.assertRaisesRegex(tool.Refused, re.escape(message)):
                    tool.read_pin(self.scripts / 'markdownlint')
        # Through the command line, a refused pin is refused before anything is written.
        self.assert_refused(self.install(), cases[-1][2])
        self.assertFalse(self.prefix.exists())

    def test_a_prefix_others_can_write_is_refused_before_fetching(self):
        self.prefix.mkdir(mode=0o700)
        self.prefix.chmod(0o777)
        shutil.rmtree(self.mirror)
        self.assert_refused(self.install(), 'writable by other users')


class CommittedPin(unittest.TestCase):
    """The repository's own pin is an exact closure of markdownlint-cli2 0.22.1."""

    def test_the_committed_pin_is_consistent(self):
        pin = tool.read_pin(SCRIPTS / 'markdownlint')
        self.assertEqual(pin['lock']['node_modules/markdownlint-cli2']['version'], '0.22.1')
        self.assertEqual(pin['lock']['node_modules/markdownlint']['version'], '0.40.0')
        self.assertRegex(pin['tree'], r'^sha256:[0-9a-f]{64}$')
        self.assertEqual(tool.BIN, 'node_modules/markdownlint-cli2/markdownlint-cli2-bin.mjs')


@unittest.skipUnless(os.environ.get('AF_MARKDOWNLINT_E2E') == '1',
                     'set AF_MARKDOWNLINT_E2E=1 to install and run the real closure')
class RealTool(unittest.TestCase):
    """The committed closure, installed for real and run offline against fixtures."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.base = Path(os.path.realpath(cls.tmp.name))
        given = os.environ.get('AF_MARKDOWNLINT_PREFIX')
        cls.prefix = Path(given) if given else cls.base / 'prefix'
        result = subprocess.run([sys.executable, '-B', str(TOOL), 'install', '--prefix',
                                 str(cls.prefix)], capture_output=True, text=True)
        assert result.returncode == 0, result.stderr
        cls.bin = re.search(r'put on PATH: (.+)', result.stdout).group(1)
        cls.node = shutil.which('node')
        assert cls.node, 'node is required'

    @classmethod
    def tearDownClass(cls):
        unseal(cls.base)
        cls.tmp.cleanup()

    def fixture(self, name, files):
        work = self.base / name
        work.mkdir()
        shutil.copy(REPOSITORY / '.markdownlint-cli2.jsonc', work)
        for path, text in files.items():
            (work / path).parent.mkdir(parents=True, exist_ok=True)
            (work / path).write_text(text)
        return work

    def environment(self, home):
        home.mkdir()
        return {'PATH': f'{self.bin}:{Path(self.node).parent}:/usr/bin:/bin', 'HOME': str(home),
                'XDG_CACHE_HOME': str(home / '.cache'), 'XDG_CONFIG_HOME': str(home / '.config'),
                'LC_ALL': 'C', 'TZ': 'UTC',
                # Any attempt to reach a registry or the network through a proxy fails.
                'HTTP_PROXY': 'http://127.0.0.1:9', 'HTTPS_PROXY': 'http://127.0.0.1:9',
                'npm_config_registry': 'http://127.0.0.1:9/', 'npm_config_offline': 'true'}

    def lint(self, work, prefix=()):
        home = self.base / f'home-{work.name}'
        environment = self.environment(home)
        # sudo resets the environment, so a prefixed run sets the check's own one again.
        command = [*prefix, 'env', '-i', *[f'{k}={v}' for k, v in environment.items()]] \
            if prefix else []
        result = subprocess.run([*command, sys.executable, '-B', str(TOOL), 'run', '**/*.md'],
                                cwd=work, env=environment, capture_output=True, text=True)
        self.assertEqual(os.listdir(home), [], 'run writes nothing to the private HOME')
        return result

    def test_valid_markdown_passes_and_invalid_markdown_fails(self):
        valid = self.fixture('valid', {
            'a.md': '# One\n\n' + 'A long line. ' * 20 + '\n',
            'node_modules/x/bad.md': '# One\n\n# Two\n',
            'notes.markdown': '# One\n\n# Two\n'})
        invalid = self.fixture('invalid', {'a.md': '# One\n', 'sub/dir/b.md': '# One\n\n# Two\n'})
        result = self.lint(valid)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('markdownlint-cli2 v0.22.1 (markdownlint v0.40.0)', result.stdout)
        self.assertIn('Linting: 1 file(s)', result.stdout)
        result = self.lint(invalid)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn('Linting: 2 file(s)', result.stdout)
        self.assertIn('sub/dir/b.md:3 error MD025', result.stdout + result.stderr)

    def test_parallel_runs_keep_their_own_results(self):
        works = [self.fixture(f'parallel-{i}', {'a.md': '# One\n' + ('\n# Two\n' if i % 2 else '')})
                 for i in range(6)]
        with ThreadPoolExecutor(6) as pool:
            codes = [result.returncode for result in pool.map(self.lint, works)]
        self.assertEqual(codes, [i % 2 for i in range(6)])

    def test_runs_without_a_network(self):
        given = os.environ.get('AF_MARKDOWNLINT_OFFLINE')
        if given:
            prefix = shlex.split(given)
        elif all(shutil.which(name) for name in ('sudo', 'unshare', 'setpriv')) and \
                subprocess.run(['sudo', '-n', 'true'], capture_output=True).returncode == 0:
            prefix = ['sudo', '-n', 'unshare', '--net', '--', 'setpriv',
                      f'--reuid={os.getuid()}', f'--regid={os.getgid()}', '--clear-groups', '--']
        else:
            self.skipTest('no network namespace: set AF_MARKDOWNLINT_OFFLINE or allow sudo -n')
        probe = subprocess.run([*prefix, sys.executable, '-c',
                                'import socket; socket.create_connection(("1.1.1.1", 443), 3)'],
                               capture_output=True, text=True)
        self.assertNotEqual(probe.returncode, 0, 'the offline prefix still reaches the network')
        identity = subprocess.run([*prefix, 'id', '-u'], capture_output=True, text=True)
        self.assertEqual(identity.stdout.strip(), str(os.getuid()), 'run as this user')
        valid = self.fixture('offline-valid', {'a.md': '# One\n'})
        invalid = self.fixture('offline-invalid', {'a.md': '# One\n\n# Two\n'})
        self.assertEqual(self.lint(valid, prefix).returncode, 0)
        self.assertEqual(self.lint(invalid, prefix).returncode, 1)

    def test_a_changed_copy_is_refused(self):
        root = Path(self.bin).parent
        prefix = self.base / 'copy'
        prefix.mkdir(mode=0o755)
        prefix.chmod(0o755)
        copy = prefix / root.name
        shutil.copytree(root, copy)
        target = copy / 'node_modules/markdownlint/package.json'
        target.parent.chmod(0o755)
        target.chmod(0o644)
        target.write_text(target.read_text() + ' ')
        target.chmod(0o444)
        target.parent.chmod(0o555)
        work = self.fixture('copy', {'a.md': '# One\n'})
        environment = {**self.environment(self.base / 'home-copy'), 'PATH':
                       f'{copy}/bin:{Path(self.node).parent}:/usr/bin:/bin'}
        result = subprocess.run([sys.executable, '-B', str(TOOL), 'run', '**/*.md'], cwd=work,
                                env=environment, capture_output=True, text=True)
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn('node_modules/markdownlint/package.json', result.stderr)
        self.assertNotIn('markdownlint-cli2 v', result.stdout)


if __name__ == '__main__':
    unittest.main()
