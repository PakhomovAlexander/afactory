"""Native nightly durable admission, restart and deployment refusal; no live effects."""
import contextlib
import importlib.util
import json
import os
import pwd
from pathlib import Path
import sys
import unittest
from types import SimpleNamespace
from unittest import mock

from tests.support import ROOT, TempDir
from perflab.values import canonical_digest, document_bytes, ContractError, sha256_bytes

sys.path.insert(0, os.path.join(ROOT, 'scripts/nightly'))
import scenario as runtime  # noqa: E402
import native_release  # noqa: E402
# `install` has no side effects at import time.
spec = importlib.util.spec_from_file_location('nightly_install', os.path.join(ROOT, 'scripts/nightly/install.py'))
install = importlib.util.module_from_spec(spec)
spec.loader.exec_module(install)

TAG = 'nightly-2026.11.16'
D = 'sha256:' + '1' * 64


@contextlib.contextmanager
def trusted_mounts():
    # The test sandbox maps root-owned mount ancestors to nobody. Model only those
    # external filesystem ownership facts; all fixture modes, links and owners stay real.
    original = Path.lstat
    def lstat(path, *args, **kwargs):
        info = original(path, *args, **kwargs)
        if str(path) in ('/', '/tmp'):
            fields = list(info)
            fields[4] = 0
            return os.stat_result(fields)
        return info
    with mock.patch.object(Path, 'lstat', lstat):
        yield


class ServiceFixture(TempDir):
    def setUp(self):
        super().setUp()
        mounts = trusted_mounts()
        mounts.__enter__()
        self.addCleanup(mounts.__exit__, None, None, None)
        self.root = Path(self.tmp).resolve()
        self.root.chmod(0o700)
        self.jobs = self.root / 'jobs'
        self.jobs.mkdir(mode=0o700)
        for name in ('queue.lock', 'worker.lock', 'nightly.toml', 'executor.toml'):
            path = self.root / name
            path.write_bytes(b'')
            path.chmod(0o600)
        self.config = dict(schema='af/nightly-service@1', root=str(self.root), repository='owner/af',
            nightly_config=str(self.root / 'nightly.toml'), nightly_config_digest=sha256_bytes(b''),
            executor_config=str(self.root / 'executor.toml'), executor_config_digest=sha256_bytes(b''),
            max_jobs=2, max_job_wall_ms=72000000, max_bytes=1 << 30)
        self.file = self.root / 'service.json'
        self.file.write_bytes(document_bytes(self.config))
        self.file.chmod(0o600)
        self.service = runtime.Service(self.file)

    def request(self, op, method='execute', tag=TAG):
        inputs = dict(subject=dict(type='text', value=tag), revision=dict(type='text', value=self.service.digest),
                      nightly_tag=dict(type='nightly_tag', value=tag))
        index = runtime.OPS.index(op)
        if index:
            inputs['evidence'] = dict(type='observation', value=self.service.observation(self.service.job(tag), runtime.OPS[index-1])[1])
        return dict(schema='af/scenario-adapter-request@5', method=method, intent_id=D, input_digest=D,
                    operation=op, destination='github:owner/af', inputs=inputs)


class Durable(ServiceFixture):
    def test_adapter_is_bounded_and_worker_reconciles_one_job(self):
        calls = []
        class Backend:
            def run(self, job, op, resumed):
                calls.append((job['nightly_tag'], op, resumed))
                return {'build': D}, 'verified fixture heavy computation'
        backend = Backend()
        for op in runtime.OPS:
            request = self.request(op, 'reconcile')
            self.assertEqual(self.service.request(request)['outcome'], 'absent')
            request['method'] = 'execute'
            with mock.patch.object(runtime, 'Native', side_effect=AssertionError('inline heavy compute')):
                self.assertEqual(self.service.request(request)['outcome'], 'done')
                self.assertEqual(runtime.Service(self.file).request(request)['outcome'], 'done')
            self.assertEqual(len(calls), runtime.OPS.index(op))
            self.assertEqual(runtime.Service(self.file).worker(backend), 'done')
            self.assertEqual(runtime.Service(self.file).worker(backend), 'idle')
            self.assertEqual(self.service.observation(self.service.job(TAG), op)[0], 'done')
        self.assertEqual([op for _, op, _ in calls], list(runtime.OPS))
        self.assertTrue(all(tag == TAG for tag, _, _ in calls))
        self.assertEqual(len(list(self.jobs.glob('nightly-????.??.??.json'))), 1)

    def test_lost_worker_response_recovers_running_phase_without_duplicate_build(self):
        self.service.request(self.request(runtime.OPS[0]))
        job = self.service.job(TAG)
        job['phases'][runtime.OPS[0]].update(state='running', started_unix_ms=runtime.now_ms())
        self.service.save(job)
        class Backend:
            def run(self, job, op, resumed):
                self_resumed.append(resumed)
                return {'build': D}, 'reconciled existing signed build'
        self_resumed = []
        self.assertEqual(runtime.Service(self.file).worker(Backend()), 'done')
        self.assertEqual(self_resumed, [True])
        self.assertEqual(runtime.Service(self.file).worker(Backend()), 'idle')

    def test_invalid_tag_changed_inputs_predecessor_or_certificate_refused(self):
        for tag in ('nightly-2026.02.29', '../nightly', 'nightly-2026.13.01'):
            with self.assertRaises(ContractError):
                self.service.request(self.request(runtime.OPS[0], tag=tag))
        request = self.request(runtime.OPS[0])
        self.service.request(request)
        request['inputs']['revision']['value'] = D
        with self.assertRaises(ContractError):
            self.service.request(request)
        job = self.service.job(TAG)
        self.service.finish(job, runtime.OPS[0], {'build': D}, 'sealed build')
        request = self.request(runtime.OPS[1])
        request['inputs']['evidence']['value']['evidence'] = 'forged'
        with self.assertRaises(ContractError):
            self.service.request(request)
        path = self.service.completion_path(TAG, runtime.OPS[0])
        value = json.loads(path.read_bytes())
        value['artifacts']['build'] = 'sha256:' + '2' * 64
        runtime.atomic(path, value, 'nightly-completion-v1.json')
        with self.assertRaises(ContractError):
            self.service.observation(self.service.job(TAG), runtime.OPS[0])

    def test_failed_incomplete_comparison_blocks_publication_and_expired_jobs(self):
        class Backend:
            def run(self, job, op, resumed):
                raise ContractError('missing family; publication blocked')
        op = runtime.OPS[0]
        self.service.request(self.request(op))
        self.assertEqual(self.service.worker(Backend()), 'failed')
        self.assertEqual(self.service.request(self.request(op, 'reconcile'))['outcome'], 'failed')
        with self.assertRaises(ContractError):
            self.service.request(self.request(runtime.OPS[1]))
        # A separate night whose elapsed budget is exhausted gets retained failure evidence.
        tag = 'nightly-2026.11.17'
        self.service.request(self.request(op, tag=tag))
        job = self.service.job(tag)
        job['created_unix_ms'] = 1
        job['deadline_unix_ms'] = 2
        self.service.save(job)
        self.assertEqual(self.service.worker(Backend()), 'failed')
        self.assertIn('wall budget exhausted', self.service.job(tag)['phases'][op]['detail'])

    def test_only_same_worker_private_configs_and_job_state_are_admitted(self):
        (self.root / 'nightly.toml').chmod(0o644)
        with self.assertRaises(ContractError):
            runtime.Service(self.file)


class Deployment(TempDir):
    def test_service_schema_parity(self):
        for name, value in runtime.wire.schemas().items():
            self.assertEqual(json.loads(Path(ROOT, 'schemas', name).read_text()), value)

    def test_key_consistency_and_stable_identity_is_separate(self):
        key = Path(ROOT, 'crates/af/keys/nightly.pub').read_text().strip()
        self.assertIn(key, Path(ROOT, 'install.sh').read_text())
        self.assertIn('0596484DC93A8A62', key)
        self.assertNotEqual(key, Path(ROOT, 'crates/af/keys/release.pub').read_text().strip())
        runtime.trusted_key(Path(ROOT, 'crates/af/keys/nightly.pub'))
        with self.assertRaises(ContractError):
            runtime.trusted_key(Path(ROOT, 'crates/af/keys/release.pub'))

    def test_prepare_emits_concrete_files_but_missing_prerequisites_refuse_install(self):
        args = SimpleNamespace(config=self.path('absent.toml'), af='/absent/af', repo=ROOT, root=self.path('jobs'),
            state=self.path('state'), output=self.path('prepared'), user='af-nightly', start_unix_ms=1800000000000)
        install.prepare(args)
        self.assertTrue(Path(args.output, 'INSTALL.txt').is_file())
        args.output = self.path('installation')
        with self.assertRaisesRegex(ContractError, 'Missing deployment prerequisites'):
            install.install(args)
        self.assertFalse(Path(args.output).exists())
        self.assertFalse(Path(args.root).exists())

    def test_immutable_source_requires_exact_sha_and_producer(self):
        fake_git = mock.Mock()
        fake_git.resolve.return_value = 'b' * 40
        with mock.patch.object(install.git_refs, 'Git', return_value=fake_git), \
                mock.patch.object(install.executor, 'producer') as producer:
            with self.assertRaisesRegex(ContractError, 'exact control_sha'):
                install.immutable_control({'control_sha': 'a' * 40}, ROOT)
            producer.assert_not_called()
            fake_git.resolve.return_value = 'a' * 40
            producer.side_effect = ContractError('installed control bytes differ')
            with self.assertRaisesRegex(ContractError, 'control bytes differ'):
                install.immutable_control({'control_sha': 'a' * 40}, ROOT)

    def test_generated_units_have_one_schedule_and_bounded_external_worker(self):
        units = install.units('/opt/control/af', '/opt/control', '/var/lib/af/state', '/var/lib/af/native', 'af-nightly')
        self.assertEqual(len(units), 4)
        text = '\n'.join(units.values())
        self.assertNotIn('OnCalendar=', text)
        self.assertNotIn('Persistent=true', text)
        self.assertIn('TimeoutStartSec=20h', text)
        self.assertIn('User=af-nightly', text)
        self.assertIn('PrivateTmp=no', text)
        self.assertIn('KillMode=control-group', text)
        self.assertIn('Conflicts=af-nightly.timer', text)
        self.assertIn('ExecStart=/var/lib/af/native/tick', text)
        self.assertNotIn('Environment=AF_NIGHTLY_SIGN_PASSWORD', text)
        ant = install.ant(install.bundle(D, 'owner/af'), 1800000000000)
        self.assertEqual(ant['schedule'], dict(kind='daily', timezone='Europe/Moscow', hour=3, minute=0, start_unix_ms=1800000000000))
        self.assertEqual(ant['execution']['per_run']['max_tokens'], 0)
        self.assertEqual(ant['execution']['rolling_tokens'], 0)
        self.assertEqual(ant['occurrence_inputs'], dict(nightly_tag='scheduled_nightly_tag'))
        for bad in ('/tmp/a b', '/tmp/x/../y', '/tmp/x//y', 'relative'):
            with self.assertRaises(ContractError):
                install.safe_path(bad)

class Prerequisites(ServiceFixture):
    def provision(self):
        import build
        self.build = build
        for name, data, mode in [('sign.key', b'never read this fixture secret', 0o600),
                                  ('sign.pub', Path(ROOT, 'crates/af/keys/nightly.pub').read_bytes(), 0o600),
                                  ('token', b'fixture', 0o600), ('qemu', b'fixture', 0o700)]:
            path = self.root / name
            path.write_bytes(data)
            path.chmod(mode)
        mirror = self.root / 'mirror'
        mirror.mkdir(mode=0o700)
        self.nightly = dict(repository='owner/af', mirror=str(mirror), builds_root=str(self.root / 'builds'),
            build_image=D, qemu_aarch64=str(self.root / 'qemu'), sign_key=str(self.root / 'sign.key'),
            public_key=str(self.root / 'sign.pub'), report=dict(executor_config=str(self.root / 'executor.toml')),
            publish=dict(github_token_file=str(self.root / 'token')))
        self.lab = dict(backend='local', core=dict(tier='nightly', cases=[]), repository='owner/af', root=str(self.root / 'lab'),
            repository_path=str(mirror), control_sha='a' * 40, build_image=D)
        self.args = SimpleNamespace(config=str(self.root / 'nightly.toml'), af='/usr/bin/python3', repo=ROOT,
            root=str(self.root / 'newjobs'), state=str(self.root / 'state'), output=str(self.root / 'install'),
            user=pwd.getpwuid(os.getuid()).pw_name, start_unix_ms=1800000000000)
        self.commands = []
        def run(argv, **kwargs):
            self.commands.append(argv)
            return SimpleNamespace(returncode=0, stdout=json.dumps(dict(commit='a' * 12)).encode())
        self.run = run
        for patcher in (mock.patch.object(build, 'load_config', return_value=self.nightly),
                        mock.patch.object(install.executor, 'load_config', return_value=self.lab),
                        mock.patch.object(install, 'immutable_control'),
                        mock.patch.object(install, 'immutable_path'),
                        mock.patch.object(install.shutil, 'which', side_effect=lambda c: '/usr/bin/' + c),
                        mock.patch.object(install.shutil, 'disk_usage', return_value=SimpleNamespace(free=install.MIN_FREE))):
            patcher.start()
            self.addCleanup(patcher.stop)

    def test_absent_key_image_storage_tools_or_private_owner_refuse_before_authority(self):
        self.provision()
        self.assertEqual(install.dependency_errors(self.args, self.run), [])
        Path(self.nightly['sign_key']).unlink()
        self.assertIn('sign_key', '\n'.join(install.dependency_errors(self.args, self.run)))
        Path(self.nightly['sign_key']).write_bytes(b'fixture')
        Path(self.nightly['sign_key']).chmod(0o600)
        def missing_image(argv, **kwargs):
            self.assertEqual(argv[:3], ['docker', 'image', 'inspect'])
            return SimpleNamespace(returncode=1)
        self.assertIn('missing pinned local build image', '\n'.join(install.dependency_errors(self.args, missing_image)))
        with mock.patch.object(install.shutil, 'disk_usage', return_value=SimpleNamespace(free=install.MIN_FREE - 1)):
            self.assertIn('storage', '\n'.join(install.dependency_errors(self.args, self.run)))
        with mock.patch.object(install.shutil, 'which', return_value=None):
            self.assertIn('minisign executable', install.dependency_errors(self.args, self.run))
        Path(self.nightly['sign_key']).chmod(0o644)
        self.assertIn('private', '\n'.join(install.dependency_errors(self.args, self.run)))
        self.assertFalse(Path(self.args.output).exists())
        self.assertFalse(Path(self.args.root).exists())
        self.assertTrue(all('add' not in argv and 'enable' not in argv for argv in self.commands))

    def test_installer_writes_reviewable_authority_only_after_prerequisites(self):
        self.provision()
        self.assertEqual(install.dependency_errors(self.args, self.run), [])
        with mock.patch.object(install, 'dependency_errors', return_value=[]):
            install.install(self.args)
        output = Path(self.args.output)
        self.assertTrue((output / 'tick').is_file())
        self.assertTrue((output / 'worker').is_file())
        self.assertTrue((output / 'review.txt').is_file())
        policy = json.loads((output / 'policy.json').read_bytes())
        self.assertEqual(policy['nightly_releases']['nightly']['repository'], 'owner/af')
        self.assertEqual(policy['adapters']['nightly']['digest'], sha256_bytes((output / 'adapter').read_bytes()))
        self.assertEqual((output.stat().st_mode & 0o777), 0o700)
        with self.assertRaises(ContractError):
            install.install(self.args)

class NativeGuards(ServiceFixture):
    def backend(self):
        backend = object.__new__(native_release.Native)
        backend.config = dict(builds_root=str(self.root / 'builds'), baseline_ref='v0.13.0',
                              build_image=D, mirror=str(self.root / 'mirror'))
        backend.service = SimpleNamespace(config={'nightly_config': str(self.root / 'nightly.toml')})
        return backend

    def test_running_build_without_record_never_executes_another_build(self):
        backend = self.backend()
        with mock.patch.object(native_release.build, 'main', side_effect=AssertionError('duplicate build')):
            with self.assertRaisesRegex(ContractError, 'no duplicate build'):
                backend.run_phase({'nightly_tag': TAG}, runtime.OPS[0], True)

    def test_source_key_refuses_before_build_reservation_or_compilation(self):
        backend = self.backend()
        mirror = mock.Mock()
        mirror.read_file.return_value = b'old public identity'
        plan = dict(status='build', head_sha='a' * 40)
        with mock.patch.object(native_release.build, 'load_config', return_value=backend.config), \
                mock.patch.object(native_release.build, 'Mirror', return_value=mirror), \
                mock.patch.object(native_release.build, 'previous_night', return_value=None), \
                mock.patch.object(native_release.build, 'make_plan', return_value=plan), \
                mock.patch.object(native_release.build, 'Night') as night:
            result = native_release.build.main(['build', '--config', 'fixture', '--date', '2026-11-16'],
                github=mock.Mock(), source_check=backend.source_key)
            self.assertEqual(result, native_release.build.EXIT_REFUSED)
            night.assert_not_called()
            mirror.read_file.assert_called_once_with('a' * 40, 'crates/af/keys/nightly.pub')

    def test_record_freezes_baseline_image_and_newly_authorized_source_identity(self):
        backend = self.backend()
        path = Path(backend.config['builds_root'], '2026-10-29', 'nightly.json')
        path.parent.mkdir(parents=True)
        raw = Path(ROOT, 'fixtures/performance/schema/nightly-build/valid-built.json').read_bytes()
        path.write_bytes(raw)
        key = Path(ROOT, 'crates/af/keys/nightly.pub').read_bytes()
        mirror = mock.Mock()
        mirror.read_file.return_value = key
        with mock.patch.object(native_release.build, 'Mirror', return_value=mirror):
            record = backend.record('nightly-2026.10.29')
            self.assertEqual(record['baseline_sha'], 'd' * 40)
            backend.config['baseline_ref'] = 'v0.15.1-rc.3'
            with self.assertRaisesRegex(ContractError, 'another baseline tag'):
                backend.record(record['tag'])
            backend.config['baseline_ref'] = 'v0.13.0'
            backend.config['build_image'] = 'sha256:' + '2' * 64
            with self.assertRaisesRegex(ContractError, 'another pinned image'):
                backend.record(record['tag'])
            backend.config['build_image'] = D
            mirror.read_file.return_value = b'old public identity'
            with self.assertRaisesRegex(ContractError, 'authorized nightly public identity'):
                backend.record(record['tag'])

    def test_publisher_requires_unchanged_report_certificate_before_remote_effect(self):
        backend = self.backend()
        record = dict(tag=TAG, status='built', baseline_sha='a' * 40, head_sha='b' * 40)
        backend.record = lambda tag: record
        backend.verified = lambda record: ({'manifest': 'fixture'}, {'comparison': 'fixture'})
        backend.certificate = mock.Mock(side_effect=ContractError('phase evidence changed after Wait'))
        with mock.patch.object(native_release.publish, 'publish', side_effect=AssertionError('unauthorized publication')):
            with self.assertRaisesRegex(ContractError, 'evidence changed'):
                backend.run_phase({'nightly_tag': TAG}, runtime.OPS[3], False)

    def test_comparison_does_not_cycle_unrelated_queue_or_exhausted_wall_allowance(self):
        backend = self.backend()
        backend.executor = dict(run_seconds=3600, cycle_seconds=60)
        backend.record = lambda tag: dict(tag=TAG, status='built', baseline_sha='a' * 40, head_sha='b' * 40)
        backend.certificate = lambda *args: None
        backend.comparison_id = lambda tag: 'comparison-fixture'
        backend.bundle = lambda tag: self.root / 'absent-export'
        backend.executor.update(root=str(self.root / 'lab'))
        held = dict(submission={'kind': 'release', 'key': TAG}, partitions=[{'request_id': 'own_core'}],
                    telemetry={'request_id': 'own_tel'}, diagnostics=[{'request_id': 'own_trace'}])
        journal = mock.Mock()
        journal.root = backend.executor['root']
        journal.names.return_value = ['unrelated']
        journal.state.return_value = dict(state='queued')
        manager = mock.MagicMock()
        manager.__enter__.return_value = journal
        job = dict(nightly_tag=TAG, deadline_unix_ms=runtime.now_ms() + 72000000)
        comparison = native_release.release_comparison
        with mock.patch.object(native_release.executor, 'Journal', return_value=manager), \
                mock.patch.object(comparison, 'preflight', return_value={'status': 'launched', 'detail': '', 'comparison': held}), \
                mock.patch.object(comparison, 'advance'), mock.patch.object(comparison, 'status', return_value=dict(status='running')):
            with self.assertRaisesRegex(ContractError, 'unrelated queued original'):
                backend.run_phase(job, runtime.OPS[1], True)
            journal.cycle.assert_not_called()
            journal.names.return_value = ['own_core']
            job['deadline_unix_ms'] = runtime.now_ms() + 1000
            with self.assertRaisesRegex(ContractError, 'complete native cycle'):
                backend.run_phase(job, runtime.OPS[1], True)
            journal.cycle.assert_not_called()
