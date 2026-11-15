"""The telemetry bench scripts' drain boundary and current fixture contract.

The drain uses two `CLOCK_MONOTONIC` readings. Current workload admission checks package
identity, exact current wire contracts and the measured binary planner without timing anything.
"""
import copy
import json
import os
import pathlib
import stat
import subprocess
import sys
import time

from perflab import telemetry_lab, toml_file
from tests.support import TempDir

overhead = telemetry_lab.script_module("scripts/telemetry-overhead.py")
class Drain(TempDir):
    def store(self, lines, reply_mtime_ns=None):
        """A Store whose CAS holds the dense harness's answer and its stderr."""
        state = pathlib.Path(self.tmp) / "store"
        objects = state / "cas" / "objects" / "ab"
        objects.mkdir(parents=True)
        reply = objects / "reply"
        reply.write_bytes(overhead.REPLY)
        if reply_mtime_ns is not None:
            os.utime(reply, ns=(reply_mtime_ns, reply_mtime_ns))
        (objects / "stderr").write_bytes(b"".join(json.dumps(line, separators=(",", ":"))
                                                  .encode() + b"\n"
                                                  for line in lines))
        return state

    def test_the_drain_is_ordered_monotonic_readings_whatever_the_file_timestamps(self):
        # A de-duplicated answer object may keep an earlier run's modification time, and the
        # wall clock may step back: neither reaches the drain, which reads only the harness's
        # and the measurement's CLOCK_MONOTONIC.
        written = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
        state = self.store([{"type": "assistant"},
                            {"type": "result", "subtype": "af_bench_last_write",
                             "last_write_unix_ns": str(time.time_ns() + 10 ** 12),
                             "last_write_monotonic_ns": str(written)}],
                           reply_mtime_ns=1)
        self.assertEqual(overhead.last_write(state), written)
        exited = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
        self.assertGreaterEqual(exited, written)
        self.assertGreaterEqual(overhead.drain_latency(written, exited), 0)
        self.assertEqual(overhead.drain_latency(1_000_000, 3_500_000), 2.5)

    def test_a_negative_raw_drain_is_kept_as_measured(self):
        self.assertEqual(overhead.drain_latency(3_000_000, 1_000_000), -2.0)

    def test_a_harness_without_a_monotonic_last_write_is_refused(self):
        # The superseded file-clock spelling is not read.
        state = self.store([{"type": "result", "subtype": "af_bench_last_write",
                             "last_write_unix_ns": "5", "last_write_fs_ns": "5"}])
        with self.assertRaisesRegex(overhead.BenchError, "CLOCK_MONOTONIC"):
            overhead.last_write(state)

    def test_judge_refuses_another_drain_boundary_or_clock(self):
        from tests import telemetry_fixtures as fx
        doc = fx.overhead_doc(20261021, 3)
        for key, value in (("boundary", "cas_commit"), ("clock", "filesystem")):
            changed = copy.deepcopy(doc)
            changed["drain"][key] = value
            with self.assertRaisesRegex(overhead.BenchError, "other boundaries"):
                overhead.summarize(changed)


class FixtureContracts(TempDir):
    def test_only_the_current_contract_is_selected_without_a_release_table(self):
        self.assertEqual(overhead.select_contract(), "native-task-review@1")
        self.assertEqual(overhead.select_contract(overhead.CURRENT_CONTRACT), overhead.CURRENT_CONTRACT)
        for retired in ("check-receipt@1", "check-receipt@2", "0.15.1-rc.1", "unknown"):
            with self.subTest(contract=retired), self.assertRaisesRegex(
                    overhead.BenchError, "unsupported fixture contract"):
                overhead.select_contract(retired)
        with self.assertRaisesRegex(overhead.BenchError, "not `af VERSION`"):
            overhead.release_version("0.15.1-rc.1")

    def test_workspace_exact_release_declaration_binds_current_capabilities(self):
        from tests.support import ROOT
        preflight = telemetry_lab.script_module("scripts/release-contract.py")
        manifest = toml_file.loads((pathlib.Path(ROOT) / "Cargo.toml").read_bytes(), "Cargo.toml")
        release = manifest["workspace"]["package"]["version"]
        self.assertEqual(preflight.check(), release)
        declaration = json.loads((pathlib.Path(ROOT) / 'scripts/release-contract.json').read_text())
        self.assertEqual(declaration['version'], release)
        self.assertEqual(declaration['capabilities'], preflight.capabilities(pathlib.Path(ROOT)))
        self.assertEqual(declaration['contract'], overhead.CURRENT_CONTRACT)
        self.assertEqual(declaration['receipt'], overhead.CHECK_RECEIPT_CURRENT)
        self.assertEqual(overhead.select_contract(), "native-task-review@1")
        self.assertEqual(overhead.CHECK_RECEIPT_CURRENT, "af/TaskCheckReceipt@2")

    def test_current_receipt_and_release_provenance_are_checked_and_old_documents_refused(self):
        from tests import telemetry_fixtures as fx
        for module, make in ((overhead, fx.overhead_doc),
                             (telemetry_lab.script_module("scripts/telemetry-invocation-cost.py"),
                              fx.invocation_doc)):
            doc = make(1, 1, version="af 0.15.1-rc.1")
            self.assertEqual(module.summarize(copy.deepcopy(doc)), doc)
            for key, value in (("name", "check-receipt@2"),
                               ("check_receipt", "af/TaskCheckReceipt@1"),
                               ("release", "0.0.0")):
                changed = copy.deepcopy(doc)
                changed['fixture_contract'][key] = value
                with self.subTest(script=module.SCHEMA, field=key), self.assertRaises(overhead.BenchError if module is overhead else module.BenchError):
                    module.summarize(changed)
            recorded = pathlib.Path(self.path('document.json'))
            doc['schema'] = module.SCHEMA[:-1] + '2'
            recorded.write_text(json.dumps(doc))
            with self.assertRaisesRegex(module.BenchError, "not an"):
                module.judge(recorded)

    def test_every_fixture_has_exact_current_package_pins_and_no_rewrite(self):
        from perflab import cli_cases
        from unittest import mock
        import re
        for source in sorted(overhead.FIXTURES_DIR.glob('*/repo')):
            with self.subTest(fixture=source.parent.name):
                self.assertEqual(cli_cases.catalog_problems(str(source)), [])
                catalog = (source / '.af/task-catalog.toml').read_text()
                pins = {path: digest for digest, path in re.findall(
                    r'digest = "([^"]+)"\npath = "([^"]+)"', catalog)}
                before = {str(p.relative_to(source)): p.read_bytes() for p in source.rglob('*')
                          if p.is_file()}
                with mock.patch.object(overhead, 'af_digest', side_effect=lambda af, repo, path: pins[path]):
                    produced = overhead.apply_contract('unused', source.parent.name, source,
                                                       overhead.CURRENT_CONTRACT)
                self.assertEqual(sorted(produced['packages'].values()), sorted(pins.values()))
                self.assertEqual(before, {str(p.relative_to(source)): p.read_bytes()
                                         for p in source.rglob('*') if p.is_file()})
                with mock.patch.object(overhead, 'af_digest', return_value='sha256:'+'0'*64):
                    with self.assertRaisesRegex(overhead.BenchError, 'not the declared'):
                        overhead.apply_contract('unused', source.parent.name, source,
                                                overhead.CURRENT_CONTRACT)

    def test_binary_planner_refusal_keeps_the_original_diagnostic_without_measured_output(self):
        from unittest import mock
        from types import SimpleNamespace
        repo = pathlib.Path(self.path('fixture/pristine'))
        repo.mkdir(parents=True)
        refused = SimpleNamespace(returncode=1, stdout=b'{"schema":"af/error@2"}',
                                  stderr=b'Pipeline af.pipeline/6 is unsupported')
        with mock.patch.object(overhead.subprocess, 'run', return_value=refused) as launch:
            with self.assertRaisesRegex(overhead.BenchError, 'Pipeline af.pipeline/6 is unsupported'):
                overhead.admit_fixture('unsupported-af', 'telemetry-bench-concurrent', repo)
        self.assertNotIn('--execute', launch.call_args.args[0])
        self.assertFalse((repo.parent / 'admission').exists())
        self.assertFalse((repo.parent / '00-off/stdout').exists())

    def test_runtime_smoke_keeps_early_exit_instead_of_reading_absent_task_output(self):
        from tests import telemetry_runtime_smoke as smoke
        from perflab.telemetry_exec import Outcome
        outcome = Outcome(1, b'', 0, b'unsupported current fixture contract: original reason', 52, 0)
        artifact = pathlib.Path(self.tmp) / 'bench/telemetry-bench-concurrent/00-off/stdout'
        for content in (None, b'not JSON'):
            with self.subTest(content=content):
                if content is not None:
                    artifact.parent.mkdir(parents=True)
                    artifact.write_bytes(content)
                with self.assertRaisesRegex(AssertionError, 'original reason') as caught:
                    smoke.failed_task_document(pathlib.Path(self.tmp), outcome)
                self.assertNotIsInstance(caught.exception, FileNotFoundError)

    def test_successful_binary_admission_requires_a_current_zero_attempt_plan(self):
        from unittest import mock
        from types import SimpleNamespace
        repo = pathlib.Path(self.path('fixture/pristine'))
        repo.mkdir(parents=True)
        ready = {'schema':'af/task-inspection@20', 'attempts':0, 'phase':{'kind':'ready'},
                 'task_id':'telemetry-bench-concurrent', 'plan_id':'sha256:'+'1'*64}
        documents = [ready, {}, dict(ready, schema='af/task-inspection@18'),
                     dict(ready, attempts=1), dict(ready, attempts=False),
                     dict(ready, phase={'kind':'finished'}), dict(ready, plan_id='not-a-plan'),
                     dict(ready, task_id='another-task')]
        for document in documents:
            done = SimpleNamespace(returncode=0, stdout=json.dumps(document).encode(), stderr=b'')
            with mock.patch.object(overhead.subprocess, 'run', return_value=done):
                if document is ready:
                    overhead.admit_fixture('current-af', 'telemetry-bench-concurrent', repo)
                else:
                    with self.assertRaisesRegex(overhead.BenchError, 'malformed current workload admission'):
                        overhead.admit_fixture('bad-af', 'telemetry-bench-concurrent', repo)

    def test_store_sequence_retains_the_first_failure_and_never_runs_the_second_task(self):
        from unittest import mock
        from types import SimpleNamespace
        pristine = pathlib.Path(self.path('pristine'))
        pristine.mkdir()
        calls = []
        def launch(argv, **kwargs):
            calls.append(argv)
            kwargs['stderr'].write(b'original native check refusal')
            return SimpleNamespace(pid=123, returncode=None)
        with mock.patch.object(overhead.subprocess, 'Popen', side_effect=launch), \
                mock.patch.object(overhead.os, 'wait4', return_value=(123, 5 << 8,
                                  SimpleNamespace(ru_maxrss=100))):
            with self.assertRaisesRegex(overhead.BenchError, 'exit 5.*expected 0\\noriginal native check refusal'):
                overhead.run_once('af', 'telemetry-bench-store', pristine,
                                  pathlib.Path(self.path('work')), '00-off', 'off')
        self.assertEqual(len(calls), 1, 'a failed first Task prevents the second Task')
        self.assertEqual(calls[0][calls[0].index('--file') + 1],
                         str(overhead.task_files('telemetry-bench-store')[0]))

    def test_native_store_preserves_1000_checks_with_bounded_graphs_and_exact_smoke_checks(self):
        from unittest import mock
        fixture = overhead.FIXTURES_DIR / 'telemetry-bench-store/repo'
        review = toml_file.loads((fixture / '.af/task-packages/bench/store/pipeline.toml').read_bytes(), 'review')
        pipeline = toml_file.loads((fixture / '.af/task-packages/bench/checks/pipeline.toml').read_bytes(), 'pipeline')
        nodes = [n for n in pipeline['nodes'] if n['operator']['op'] == 'check']
        self.assertEqual(2 * sum(len(n['operator']['checks']) for n in nodes), 1000)
        self.assertEqual(len(overhead.task_files('telemetry-bench-store')), 2)
        self.assertLessEqual(len(pipeline['nodes']) + len(review['nodes']) + 2, 64)
        self.assertEqual(len(review['slots']), 4)
        self.assertEqual(review['max_parallel'], 4)
        policy = toml_file.loads((fixture / '.af/code-policy.toml').read_bytes(), 'policy')
        self.assertEqual(set(nodes[-1]['operator']['checks']), set(policy['checks']))
        repo = pathlib.Path(self.path('smoke'))
        overhead.copy_writable(fixture, repo)
        with mock.patch.object(overhead, 'af_digest', return_value='sha256:'+'0'*64):
            overhead.keep_checks('unused', repo, 10)
        smoke = toml_file.loads((repo / '.af/task-packages/bench/checks/pipeline.toml').read_bytes(), 'smoke')
        checks = [n for n in smoke['nodes'] if n['operator']['op'] == 'check']
        self.assertEqual(len(checks), 1)
        self.assertEqual(2 * len(checks[0]['operator']['checks']), 10)
        policy = toml_file.loads((repo / '.af/code-policy.toml').read_bytes(), 'policy')
        self.assertEqual(set(checks[0]['operator']['checks']), set(policy['checks']))

class AccountIsolation(TempDir):
    def test_declared_relative_inputs_and_outputs_survive_reconstructed_cwd(self):
        if sys.platform != 'linux':
            self.skipTest('Linux user/mount namespace contract')
        from tests.account_namespace_smoke import foreign_ancestor
        from perflab import account_isolation
        parent = pathlib.Path(self.path('ancestor'))
        for name in ('home', 'cwd', 'inputs'):
            (parent / name).mkdir(parents=True, mode=0o700)
        source = parent / 'inputs/source'
        source.write_text('required source')
        source.chmod(0o444)
        program = '''
import argparse, pathlib
p = argparse.ArgumentParser()
p.add_argument('--input'); p.add_argument('--report')
a = p.parse_args()
pathlib.Path(a.report).write_text(pathlib.Path(a.input).read_text())
'''
        cwd = parent / 'cwd'
        # Both inputs and output are outside the bound cwd, with the original joined
        # and relative syntax. Explicit metadata supplies their independent branches.
        command = [sys.executable, '-I', '-c', program,
                   '--input=../inputs/source', '--report=../report']
        before = source.stat()
        with foreign_ancestor(parent, 0o755):
            with account_isolation.namespace_argv(
                    parent / 'home', command, cwd=cwd, inputs=('../inputs/source',),
                    outputs=('../report',)) as argv:
                self.assertEqual(argv[argv.index('--') + 1:], command)
                result = subprocess.run(argv, cwd=cwd, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((parent / 'report').read_text(), 'required source')
        self.assertTrue(os.path.samestat(before, source.stat()))
        with self.assertRaises(FileNotFoundError):
            with account_isolation.namespace_argv(
                    parent / 'home', command, cwd=cwd, inputs=('../inputs/missing',)):
                self.fail('missing required input admitted')

    def test_directory_declarations_never_inspect_synthetic_entry_names(self):
        if sys.platform != 'linux':
            self.skipTest('Linux user/mount namespace contract')
        from unittest import mock
        from perflab import account_isolation
        home = pathlib.Path(self.path('home'))
        home.mkdir(mode=0o700)
        original = pathlib.Path.stat
        for failure in (PermissionError, FileNotFoundError):
            def guarded(path, *args, **kwargs):
                if path.name == 'entry':
                    raise failure('unrelated inaccessible/disappearing entry')
                return original(path, *args, **kwargs)
            with mock.patch.object(pathlib.Path, 'stat', guarded):
                with account_isolation.namespace_argv(home, ['/usr/bin/true']) as argv:
                    result = subprocess.run(argv, capture_output=True)
                    self.assertEqual(result.returncode, 0, result.stderr)

    def test_parent_components_follow_symlinks_without_remounting_ancestors(self):
        if sys.platform != 'linux':
            self.skipTest('Linux user/mount namespace contract')
        from tests.account_namespace_smoke import foreign_ancestor
        from perflab import account_isolation
        parent = pathlib.Path(self.path('ancestor'))
        for name in ('home', 'work', 'nested/target', 'nested/tools'):
            (parent / name).mkdir(parents=True, mode=0o700)
        (parent / 'route').symlink_to('nested/target')
        (parent / 'alias').symlink_to('route/../tools')
        (parent / 'nested/tools/tool').symlink_to(sys.executable)
        cwd = parent / 'work'
        env = dict(os.environ, PATH='../alias:/opt/../usr/bin:/usr/bin')
        program = ('import os,pathlib; '
                   'assert all(p.stat().st_uid == os.geteuid() '
                   'for p in pathlib.Path(os.environ["HOME"]).parents)')
        env['HOME'] = str(parent / 'home')
        for executable in ('tool', '../alias/tool'):
            command = [executable, '-I', '-c', program]
            with foreign_ancestor(parent, 0o711):
                with account_isolation.namespace_argv(
                        parent / 'home', command, cwd=cwd, env=env) as argv:
                    targets = [argv[i + 2] for i, word in enumerate(argv)
                               if word in ('--bind', '--dev-bind', '--ro-bind', '--symlink')]
                    self.assertFalse(any('..' in pathlib.Path(p).parts for p in targets))
                    result = subprocess.run(argv, cwd=cwd, env=env, capture_output=True)
                    self.assertEqual(result.returncode, 0, result.stderr)

    def test_actual_child_retains_separate_tmp_volume_and_cleans_backings(self):
        if sys.platform != 'linux':
            self.skipTest('Linux user/mount namespace contract')
        from tests.support import ROOT
        # The outer fixture supplies a genuinely distinct original /tmp filesystem,
        # irrespective of the host layout. HOME stays on the checkout's real device.
        import tempfile
        target = pathlib.Path(ROOT) / 'target'
        target.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=target) as directory:
            home = pathlib.Path(directory) / 'home'
            home.mkdir(mode=0o700)
            program = '''
import json, os, pathlib, subprocess, sys
sys.path.insert(0, str(pathlib.Path(sys.argv[1]) / 'scripts/performance'))
from perflab import account_isolation
home = pathlib.Path(sys.argv[2])
def volume(p):
    v = os.statvfs(p)
    return [os.stat(p).st_dev, v.f_blocks, v.f_frsize]
before = volume('/tmp')
assert before[0] != volume(home)[0]
code = "import os,json; v=os.statvfs('/tmp'); print(json.dumps([os.stat('/tmp').st_dev,v.f_blocks,v.f_frsize]))"
with account_isolation.namespace_argv(home, [sys.executable, '-I', '-c', code]) as argv:
    backings = [pathlib.Path(argv[i+1]) for i, v in enumerate(argv) if v == '--bind' and 'af-namespace-' in argv[i+1]]
    r = subprocess.run(argv, capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    assert json.loads(r.stdout) == before, (r.stdout, before)
assert all(not p.exists() for p in backings), backings
'''
            result = subprocess.run([
                'bwrap', '--unshare-user', '--uid', str(os.geteuid()), '--gid', str(os.getegid()),
                '--bind', '/', '/', '--tmpfs', '/tmp', '--', sys.executable, '-I', '-B', '-c',
                program, ROOT, str(home)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_unavailable_same_volume_backing_refuses_and_removes_preparation(self):
        if sys.platform != 'linux':
            self.skipTest('Linux user/mount namespace contract')
        from unittest import mock
        from perflab import account_isolation
        home = pathlib.Path(self.path('home'))
        home.mkdir(mode=0o700)
        with mock.patch.object(account_isolation.os, 'access', return_value=False):
            with self.assertRaisesRegex(ValueError, 'no writable backing on measurement filesystem'):
                with account_isolation.namespace_argv(home, ['/usr/bin/true']):
                    self.fail('unavailable original filesystem admitted a launch')
        self.assertEqual(list(home.iterdir()), [], 'failed preparation must remove its account files')

    def test_private_namespace_preserves_files_and_account_with_unmapped_ancestors(self):
        from perflab import account_isolation
        if sys.platform != 'linux':
            self.skipTest('Linux user/mount namespace contract')
        home = pathlib.Path(self.path('home'))
        home.mkdir(mode=0o700)
        source = pathlib.Path(self.path('source'))
        source.write_text('source bytes stay on the original inode')
        source.chmod(0o444)
        paths = [home, source, pathlib.Path(sys.executable).resolve()]
        identities = [(p.stat().st_dev, p.stat().st_ino, stat.S_IMODE(p.stat().st_mode))
                      for p in paths]
        volumes = [(os.statvfs(p).f_blocks, os.statvfs(p).f_frsize) for p in paths]
        parents = list(home.parents)
        modes = [stat.S_IMODE(p.stat().st_mode) for p in parents]
        original_owners = [p.stat().st_uid for p in parents]
        devices = [p.stat().st_dev for p in parents]
        program = '''
import json, os, pathlib, pwd, stat, sys
paths = [pathlib.Path(p) for p in sys.argv[1:]]
home = paths[0]
assert pwd.getpwuid(os.geteuid()).pw_dir == str(home)
assert pathlib.Path('/etc/nsswitch.conf').read_text().startswith('passwd: files\\n')
assert paths[1].read_text() == 'source bytes stay on the original inode'
print(json.dumps({
    'identities': [(p.stat().st_dev, p.stat().st_ino, stat.S_IMODE(p.stat().st_mode)) for p in paths],
    'volumes': [(os.statvfs(p).f_blocks, os.statvfs(p).f_frsize) for p in paths],
    'owners': [p.stat().st_uid for p in home.parents],
    'modes': [stat.S_IMODE(p.stat().st_mode) for p in home.parents],
    'devices': [p.stat().st_dev for p in home.parents],
    'executable_owner': paths[2].stat().st_uid,
}))
'''
        with account_isolation.namespace_argv(
                str(home), [sys.executable, '-I', '-c', program, *map(str, paths)],
                inputs=paths) as argv:
            result = subprocess.run(argv, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        observed = json.loads(result.stdout)
        self.assertEqual(observed['identities'], [list(identity) for identity in identities])
        self.assertEqual(observed['volumes'], [list(volume) for volume in volumes])
        self.assertEqual(observed['modes'], modes)
        self.assertEqual(observed['owners'], [os.geteuid()] * len(parents))
        for index, device in enumerate(devices):
            if device == home.stat().st_dev:
                self.assertEqual(observed['devices'][index], device,
                                 'private account storage must use its original filesystem')
        # A private directory mount is a new inode, not a remapping of arbitrary
        # overflow ownership. Foreign executable leaves remain foreign, and AF's
        # admission rules must still refuse those leaves when ownership matters.
        if paths[2].stat().st_uid != os.geteuid():
            self.assertNotEqual(observed['executable_owner'], os.geteuid())
        self.assertEqual([p.stat().st_uid for p in parents], original_owners)

    def test_search_only_and_populated_ancestors_launch_without_sibling_inventory(self):
        from tests.account_namespace_smoke import foreign_ancestor
        from perflab import account_isolation
        if sys.platform != 'linux':
            self.skipTest('Linux user/mount namespace contract')
        parent = pathlib.Path(self.path('ancestor'))
        parent.mkdir()
        home = parent / 'home'
        home.mkdir(mode=0o700)
        for mode in (0o711, 0o755):
            with foreign_ancestor(parent, mode):
                with account_isolation.namespace_argv(home, ['/usr/bin/true']) as argv:
                    result = subprocess.run(argv, capture_output=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
        # This exceeds exec ARG_MAX with the former three arguments per sibling.
        for index in range(6000):
            (parent / ('unrelated-%04d-' % index + 'x' * 210)).touch()
        with foreign_ancestor(parent, 0o755):
            with account_isolation.namespace_argv(home, ['/usr/bin/true']) as argv:
                self.assertLess(sum(len(os.fsencode(arg)) + 1 for arg in argv), 65536)
                self.assertFalse(any('unrelated-' in arg for arg in argv))
                result = subprocess.run(argv, capture_output=True)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_relative_command_and_path_symlink_branches_use_declared_cwd(self):
        from unittest import mock
        from tests.account_namespace_smoke import foreign_ancestor
        from perflab import account_isolation
        if sys.platform != 'linux':
            self.skipTest('Linux user/mount namespace contract')
        parent = pathlib.Path(self.path('ancestor'))
        parent.mkdir()
        for name in ('home', 'work', 'tools'):
            (parent / name).mkdir(mode=0o700)
        (parent / 'tools/tool').symlink_to('/usr/bin/true')
        (parent / 'alias').symlink_to('tools')
        cwd = parent / 'work'
        env = dict(os.environ, PATH='../alias:/usr/bin:/bin')
        for command in (['tool'], ['../alias/tool'], [str(parent / 'alias/tool')]):
            with foreign_ancestor(parent, 0o711), mock.patch.object(
                    pathlib.Path, 'iterdir', side_effect=AssertionError('sibling inventory')):
                with account_isolation.namespace_argv(
                        parent / 'home', command, env=env, cwd=cwd) as argv:
                    self.assertEqual(argv[argv.index('--') + 1:], command)
                    result = subprocess.run(argv, env=env, cwd=cwd, capture_output=True)
                    self.assertEqual(result.returncode, 0, result.stderr)

    def test_contained_scripts_receive_the_account_helper_in_their_declared_readable_inputs(self):
        from tests.support import ROOT
        registry, _ = telemetry_lab.load_registry()
        helper = os.path.join(ROOT, 'scripts/performance/perflab/account_isolation.py')
        for experiment in registry['experiments']:
            readable = telemetry_lab._readable(registry, ROOT, experiment, None)
            self.assertIn(helper, readable, experiment)
        self.assertIn('scripts/performance/perflab', telemetry_lab.authority_names(registry))

    def test_guarded_builders_retain_only_the_validated_fixture_marker(self):
        from unittest import mock
        from perflab import account_isolation
        home = pathlib.Path(self.path('home'))
        home.mkdir(mode=0o700)
        modules = [overhead, telemetry_lab.script_module('scripts/telemetry-corpus.py')]
        for marker in (str(home), str(home / '..'), '/', 'relative'):
            parent = {'HOME': str(home), 'AF_TEST_WAKE_ACCOUNT_HOME': marker,
                      'AF_PROVIDERS_FILE': '/foreign', 'AF_BUDGET': 'foreign'}
            with mock.patch.dict(os.environ, parent, clear=True):
                for module in modules:
                    make = lambda: module.private_env(home, 'off') if module is overhead else module.private_env(home)
                    if marker == str(home):
                        env = make()
                        self.assertEqual(env[account_isolation.FIXTURE], str(home))
                        self.assertNotIn('AF_PROVIDERS_FILE', env)
                        self.assertNotIn('AF_BUDGET', env)
                    else:
                        with self.assertRaises((ValueError, FileNotFoundError)):
                            make()
        home.chmod(0o755)
        with self.assertRaisesRegex(ValueError, 'owner-private'):
            account_isolation.fixture_environment({}, {'HOME': str(home), account_isolation.FIXTURE: str(home)})

    def test_release_launcher_changes_actual_os_account_before_measurement_script(self):
        from unittest import mock
        from perflab import account_isolation, telemetry_exec
        home = pathlib.Path(self.path('home'))
        home.mkdir(mode=0o700)
        command = ['/usr/bin/python3', '-I', 'telemetry-overhead.py', '--af', '/private/release-af']
        with account_isolation.namespace_argv(str(home), command) as argv:
            mounts = [(argv[i + 1], argv[i + 2]) for i, word in enumerate(argv) if word == '--ro-bind']
            passwd, nss = [pathlib.Path(source).read_text() for source, target in mounts]
        self.assertEqual(argv[:2], ['bwrap', '--unshare-user'])
        self.assertEqual(argv[argv.index('--') + 1:], command)
        self.assertIn(':%d:%d:AF measurement:%s:' % (os.geteuid(), os.getegid(), home), passwd)
        self.assertEqual([target for _, target in mounts], ['/etc/passwd', '/etc/nsswitch.conf'])
        self.assertIn('passwd: files\n', nss)
        # A release laboratory execution uses the namespace even though its private debug
        # fixture marker is present. The actual executable argv and wait4 interval inside
        # the script are unchanged; there is no per-AF wrapper in the measured interval.
        execution = type('Execution', (), dict(argv=command, cwd=str(home),
            env={'HOME': str(home), account_isolation.FIXTURE: str(home)}, file_bytes=1 << 30,
            readable=[], writable=[str(home)], capture=str(home)))()
        with mock.patch.object(account_isolation.sys, 'platform', 'linux'), mock.patch.object(
                telemetry_exec.subprocess, 'Popen', side_effect=OSError('recorded')) as popen:
            with self.assertRaises(OSError):
                telemetry_exec.TrustedLocal('/usr/bin/python3').launch(execution, None, None)
            called = popen.call_args.args[0]
            self.assertEqual(called[0], 'bwrap')
            self.assertEqual(called[called.index('--') + 1:], command)


if __name__ == "__main__":
    import unittest
    unittest.main()
