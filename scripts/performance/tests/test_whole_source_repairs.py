"""Whole-source regressions. All latency figures are synthetic, never measurements."""
import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import threading
import time
from unittest import mock

from perflab import budget, bundle, contracts, gitrefs, report, telemetrycollect, telemetrydiag, telemetrylab
from perflab.values import ContractError, sha256_bytes
from tests import telemetry_fixtures as fx
from tests import test_telemetry_collect as tc
from tests import test_telemetry_diagnostics as td
from tests import test_telemetry_lab as tl
from tests.support import ROOT, TempDir, read


class Corpus(TempDir):
    setUp = tl.Authority.setUp

    def full_bundle(self):
        result = copy.deepcopy(self.result)
        result['configuration']['read_latency']['runs'] = 10000
        result['configuration']['digest'] = contracts.telemetry_configuration_digest(result['configuration'])
        result['corpus']['runs'] = 10000
        doc = fx.latency_doc(10000)
        result['corpus']['facts'] = {key: doc['corpus'][key] for key in telemetrylab.CORPUS_FACTS}
        documents = {}
        for run in result['runs']:
            data = (json.dumps(doc) + '\n').encode()
            Path(self.output, run['document']['path']).write_bytes(data)
            fx.rehash(run, data)
            run['judgment'] = {'verdict': 'pass', 'reasons': []}
            documents[(run['experiment'], run['round'], run['side'])] = copy.deepcopy(doc)
        result['within'], result['comparisons'] = telemetrylab.derive(result, documents)
        fx.rewrite(self.output, result)
        return result

    def test_every_generator_count_and_content_index_is_required_online_and_offline(self):
        result = self.full_bundle()
        self.assertEqual(telemetrylab.verify(self.output)[0], result)
        registry, _ = telemetrylab.load_registry()
        valid = fx.latency_doc(10000)
        fields = ('executions', 'spans', 'events', 'blobs', 'shared_blobs', 'blob_occurrences',
                  'blob_bytes', 'content_index', 'content_indexed')
        for field in fields:
            with self.subTest(field=field):
                doc = copy.deepcopy(valid)
                doc['corpus'][field] = False if field == 'content_index' else 0
                data = json.dumps(doc).encode()
                admitted = telemetrylab.read_document('read_latency', data, 'synthetic', registry)
                with self.assertRaisesRegex(ContractError, 'generator protocol'):
                    telemetrylab.admit('read_latency', admitted, fx.expect('read_latency', runs=10000), registry)
                forged = copy.deepcopy(result)
                for run in forged['runs']:
                    Path(self.output, run['document']['path']).write_bytes(data)
                    fx.rehash(run, data)
                fx.rewrite(self.output, forged)
                with self.assertRaisesRegex(ContractError, 'generator protocol'):
                    telemetrylab.verify(self.output)
        # Partial indexing is refused even when nonzero and the enable bit is true.
        doc = copy.deepcopy(valid)
        doc['corpus']['content_indexed'] -= 1
        with self.assertRaisesRegex(ContractError, 'content_indexed'):
            telemetrylab.admit('read_latency', doc, fx.expect('read_latency', runs=10000), registry)
        old = copy.deepcopy(valid)
        old['schema'] = 'af/telemetry-read-latency@1'
        del old['corpus']['content_indexed']
        with self.assertRaises(ContractError):
            telemetrylab.read_document('read_latency', json.dumps(old).encode(), 'prior shape', registry)

    def test_execution_admission_refuses_dropped_content_before_deriving_comparisons(self):
        def dropped(execution):
            argv = execution.argv
            facts = telemetrylab.corpus_facts(argv[argv.index('--dir') + 1])
            doc = fx.latency_doc(10, facts=facts)
            doc['corpus']['events'] = 0
            data = json.dumps(doc).encode()
            from perflab import telemetryexec
            return telemetryexec.Outcome(0, data, len(data), b'', 0, 1)
        result = telemetrylab.run(fx.settings(self, fx.StandIn(self.binaries, {
            ('read_latency', 'candidate'): dropped}), 'dropped', experiments=['read_latency']))
        candidates = [run for run in result['runs'] if run['side'] == 'candidate']
        self.assertTrue(all(run['status'] == 'refused_document' for run in candidates))
        self.assertTrue(all(item['state'] == 'missing' for item in result['comparisons']))


class Authority(TempDir):
    setUp = tl.Authority.setUp

    def copy_authority(self):
        registry, _ = telemetrylab.load_registry()
        root = self.path('authority')
        for relative in telemetrylab.authority_files(registry):
            path = Path(root, relative)
            path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(Path(ROOT, relative), path)
        return registry, root

    def test_transitive_admission_arithmetic_bundle_render_build_and_containment_are_identity(self):
        registry, root = self.copy_authority()
        original = telemetrylab.authority_digest(registry, root)
        for name in ('contracts', 'values', 'spec', 'bundle', 'render', 'builds', 'supervise', 'budget'):
            with self.subTest(name=name):
                path = Path(root, 'scripts/performance/perflab', name + '.py')
                self.assertTrue(path.is_file())
                old = path.read_bytes()
                path.write_bytes(old + b'\n# different semantics\n')
                digest, count = telemetrylab.authority_digest(registry, root)
                self.assertNotEqual(digest, original[0])
                other = copy.deepcopy(self.result)
                other['authority']['digest'] = digest
                self.assertNotEqual(telemetrylab.group_id(self.result), telemetrylab.group_id(other))
                fx.rewrite(self.output, other)
                with self.assertRaisesRegex(ContractError, 'installed verifier does not match'):
                    telemetrylab.verify(self.output, root)
                path.write_bytes(old)
        fx.rewrite(self.output, self.result)
        self.assertEqual(telemetrylab.verify(self.output, root)[0], self.result)

    def test_hashed_source_is_executed_even_when_a_matching_untracked_bytecode_cache_exists(self):
        import importlib.util
        import py_compile
        registry, root = self.copy_authority()
        path = Path(root, 'scripts/telemetry-corpus.py')
        original = path.read_bytes()
        stamp = path.stat().st_mtime
        poisoned = b"raise AssertionError('untracked bytecode executed')\n"
        path.write_bytes(poisoned + b'#' * (len(original) - len(poisoned)))
        os.utime(path, (stamp, stamp))
        cache = importlib.util.cache_from_source(str(path))
        py_compile.compile(str(path), cfile=cache, doraise=True)
        path.write_bytes(original)
        os.utime(path, (stamp, stamp))
        self.assertEqual(telemetrylab.authority_digest(registry, root),
                         telemetrylab.authority_digest(registry))
        self.assertEqual(telemetrylab.verify(self.output, root)[0], self.result)
        # A direct trusted script load also compiles source, never its parent's cache.
        self.assertEqual(telemetrylab.script_module('scripts/telemetry-corpus.py', root).FULL_RUNS, 10000)

    def test_installed_verifier_refuses_poisoned_cached_helpers_before_import(self):
        import importlib.util
        import py_compile
        registry, root = self.copy_authority()
        path = Path(root, 'scripts/performance/perflab/values.py')
        original = path.read_bytes()
        stamp = path.stat().st_mtime
        poisoned = b"raise RuntimeError('UNHASHED_HELPER_EXECUTED')\n"
        path.write_bytes(poisoned + b'#' * (len(original) - len(poisoned)))
        os.utime(path, (stamp, stamp))
        py_compile.compile(str(path), cfile=importlib.util.cache_from_source(str(path)), doraise=True)
        path.write_bytes(original)
        os.utime(path, (stamp, stamp))
        result = subprocess.run([sys.executable, '-I', '-B', str(Path(root, 'scripts/performance/cli.py')),
                                 '--help'], capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b'bytecode cache refused', result.stderr)
        self.assertNotIn(b'UNHASHED_HELPER_EXECUTED', result.stderr)
        with self.assertRaisesRegex(ContractError, 'bytecode cache'):
            telemetrylab.verify(self.output, root)

    def test_retained_authority_is_complete_budgeted_and_required_for_history(self):
        root = self.path('history')
        history = telemetrylab.history(root, [self.output])
        entry = history['results'][0]
        authority = Path(root, entry['authority_path'])
        registry, _ = telemetrylab.load_registry()
        self.assertEqual(telemetrylab.authority_digest(registry, str(authority)),
                         telemetrylab.authority_digest(registry))
        self.assertEqual(telemetrylab.verify(self.output, str(authority))[0], self.result)
        path = authority / 'scripts/performance/perflab/contracts.py'
        path.unlink()
        history = telemetrylab.history(root)
        self.assertEqual(history['results'], [])
        self.assertTrue(history['refused'])
        # An incomplete retained tree is never silently recreated from a result bundle.
        self.assertFalse(path.exists())


class Deadlines(TempDir):
    setUp = tc.Collection.setUp
    result = tc.Collection.result

    def bounded_cycle(self, artifacts, seconds=1.0, reserve=0.4):
        api = tc.transport.Collector(tc.FakeApi([tc.run_entry(1)], artifacts), tc.REPOSITORY,
                                     tc.TOKEN, api_url=tc.API, seconds=seconds)
        limit = budget.Budget(api.deadline, api.clock, reserve)
        return telemetrycollect.Cycle(self.config, api, gitrefs.Git(self.repo.path), time.time,
                                      tc.NOW, budget=limit).run()

    def assert_incomplete(self, doc):
        self.assertEqual(doc['status'], 'failed')
        self.assertIsNone(doc['results'])
        self.assertIsNone(doc['used_bytes'])
        persisted = contracts.read_file(os.path.join(self.config['root'], 'collection.json'),
                                        'telemetry-collection')
        self.assertEqual(persisted['status'], 'failed')
        self.assertIsNone(persisted['results'])
        with telemetrylab.HistoryRoot(self.config['root']):
            pass  # the previous command released the lock

    def test_http_deadline_revokes_blocked_dns_before_terminal_record_and_lock_release(self):
        release, entered, exchanges = threading.Event(), threading.Event(), []
        original_run = tc.transport._Exchange.run

        def resolve(*args):
            entered.set()
            if not release.wait(5):
                raise AssertionError('resolver was never released')
            return [(2, 1, 6, '', ('127.0.0.1', 443))]

        def running(exchange):
            exchanges.append(exchange)
            original_run(exchange)

        api = tc.transport.Collector(tc.transport.HttpsTransport(), tc.REPOSITORY,
                                     tc.TOKEN, api_url=tc.API, seconds=2.5)
        limit = budget.Budget(api.deadline, api.clock, 1.5)
        started = time.monotonic()
        with mock.patch.object(tc.transport.socket, 'getaddrinfo', resolve), \
                mock.patch.object(tc.transport.socket, 'socket') as sockets, \
                mock.patch.object(tc.transport._Exchange, 'run', running):
            try:
                doc = telemetrycollect.Cycle(self.config, api, gitrefs.Git(self.repo.path),
                                             time.time, tc.NOW, budget=limit).run()
                self.assertTrue(entered.is_set())
                self.assertLess(time.monotonic() - started, 2.5)
                self.assertTrue(exchanges[0].aborted)
                self.assertFalse(exchanges[0].done.is_set())
                self.assertIn('cancelled worker may remain', doc['detail'])
                self.assert_incomplete(doc)
            finally:
                release.set()
                if exchanges:
                    self.assertTrue(exchanges[0].done.wait(1))
            sockets.assert_not_called()

    def test_stalled_authority_git_process_is_terminated_with_the_cycle_budget(self):
        data = tc.zipped(self.result(1))
        artifacts = {1: [tc.artifact_entry(1, 1, 'perf-telemetry-1-1', data)]}
        real_run = subprocess.run
        pids = self.path('owned-pid')
        observed = []
        def stall(argv, **kwargs):
            if argv[3:5] == ['cat-file', '--batch']:
                observed.append(kwargs.get('timeout'))
                script = 'import os,time;open(%r,"w").write(str(os.getpid()));time.sleep(30)' % pids
                return real_run([sys.executable, '-I', '-B', '-c', script], **kwargs)
            return real_run(argv, **kwargs)
        started = time.monotonic()
        with mock.patch.object(gitrefs.subprocess, 'run', side_effect=stall):
            doc = self.bounded_cycle(artifacts)
        self.assertLess(time.monotonic() - started, 2.5)
        self.assertTrue(observed)
        self.assertTrue(all(value is not None and value <= 1 for value in observed))
        pid = int(Path(pids).read_text())
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)
        self.assert_incomplete(doc)
        self.assertFalse(os.path.exists(os.path.join(self.config['root'], 'results', 'tl_1_1')))

    def test_expiry_interrupts_verification_retention_and_history_instead_of_accepting_partials(self):
        data = tc.zipped(self.result(1))
        artifacts = {1: [tc.artifact_entry(1, 1, 'perf-telemetry-1-1', data)]}
        for owner, name in ((telemetrylab, 'verify'), (telemetrylab, 'materialize_authority'),
                            (telemetrylab.HistoryRoot, 'retain_authority'),
                            (telemetrylab.HistoryRoot, 'prune'), (telemetrylab, 'history_page')):
            with self.subTest(step=name):
                shutil.rmtree(self.config['root'], ignore_errors=True)
                if name == 'history_page':
                    # Isolate report expiry from import cost: this cycle starts with an
                    # already verified result; the import deadline is tested above.
                    api = tc.transport.Collector(tc.FakeApi([tc.run_entry(1)], artifacts), tc.REPOSITORY,
                                                 tc.TOKEN, api_url=tc.API)
                    self.assertEqual(telemetrycollect.Cycle(self.config, api, gitrefs.Git(self.repo.path),
                                     time.time, tc.NOW).run()['status'], 'ok')
                entered = []
                def stalled(*args, **kwargs):
                    entered.append(True)
                    time.sleep(30)
                started = time.monotonic()
                with mock.patch.object(owner, name, side_effect=stalled):
                    doc = self.bounded_cycle(artifacts)
                self.assertTrue(entered)
                self.assertLess(time.monotonic() - started, 2.5)
                self.assert_incomplete(doc)
                accepted = os.path.isdir(os.path.join(self.config['root'], 'results', 'tl_1_1'))
                # history rendering follows atomic import: this is a fully verified result,
                # but no complete history accounting is claimed for the expired cycle.
                self.assertEqual(accepted, name == 'history_page')

    def test_expiry_during_retention_keeps_the_exact_known_pruning_record(self):
        data = tc.zipped(self.result(1))
        artifacts = {1: [tc.artifact_entry(1, 1, 'perf-telemetry-1-1', data)]}
        api = tc.transport.Collector(tc.FakeApi([tc.run_entry(1)], artifacts), tc.REPOSITORY,
                                     tc.TOKEN, api_url=tc.API)
        self.assertEqual(telemetrycollect.Cycle(self.config, api, gitrefs.Git(self.repo.path),
                         time.time, tc.NOW).run()['status'], 'ok')
        self.config['retention_days'] = 1
        original = telemetrylab.HistoryRoot.remove
        entered = []
        def stalled(kept, name, reason):
            original(kept, name, reason)
            entered.append(True)
            time.sleep(30)
        api = tc.transport.Collector(tc.FakeApi([], {}), tc.REPOSITORY, tc.TOKEN,
                                     api_url=tc.API, seconds=1)
        limit = budget.Budget(api.deadline, api.clock, 0.4)
        with mock.patch.object(telemetrylab.HistoryRoot, 'remove', stalled):
            doc = telemetrycollect.Cycle(self.config, api, gitrefs.Git(self.repo.path),
                                        lambda: 4102444800.0, tc.NOW, budget=limit).run()
        self.assertTrue(entered)
        self.assertEqual([item['result_id'] for item in doc['pruned']], ['tl_1_1'])
        self.assert_incomplete(doc)
        self.assertFalse(os.path.exists(os.path.join(self.config['root'], 'results', 'tl_1_1')))

    def test_terminal_validation_is_bounded_and_any_exception_releases_the_lock(self):
        original = telemetrycollect.Cycle.document
        entered = []
        def stalled(cycle, *args, **kwargs):
            entered.append(True)
            time.sleep(30)
            return original(cycle, *args, **kwargs)
        started = time.monotonic()
        with mock.patch.object(telemetrycollect.Cycle, 'document', stalled):
            doc = self.bounded_cycle({})
        self.assertTrue(entered)
        self.assertLess(time.monotonic() - started, 2.5)
        self.assertIsNone(doc['ended_unix_ms'])
        self.assert_incomplete(doc)
        with mock.patch.object(telemetrycollect.Cycle, 'document', side_effect=RuntimeError('validation broken')):
            with self.assertRaisesRegex(RuntimeError, 'validation broken'):
                self.bounded_cycle({})
        with telemetrylab.HistoryRoot(self.config['root']):
            pass

    def test_expiry_during_post_import_cleanup_reports_the_verified_atomic_acceptance(self):
        data = tc.zipped(self.result(1))
        artifacts = {1: [tc.artifact_entry(1, 1, 'perf-telemetry-1-1', data)]}
        original = shutil.rmtree
        entered = []
        def stalled(path, *args, **kwargs):
            if str(path).split('/')[-1].startswith('.staging-collect-'):
                entered.append(True)
                time.sleep(30)
            return original(path, *args, **kwargs)
        started = time.monotonic()
        with mock.patch.object(shutil, 'rmtree', side_effect=stalled):
            doc = self.bounded_cycle(artifacts, seconds=3.0, reserve=0.5)
        self.assertTrue(entered)
        self.assertLess(time.monotonic() - started, 4)
        self.assertEqual(doc['status'], 'failed')
        self.assertIsNone(doc['results'])
        self.assertEqual([(run['outcome'], run['result_id']) for run in doc['runs']],
                         [('collected', 'tl_1_1')])
        kept = os.path.join(self.config['root'], 'results', 'tl_1_1')
        self.assertEqual(telemetrylab.verify(kept)[0]['result_id'], 'tl_1_1')
        self.assert_incomplete(doc)

    def test_cleanup_is_bounded_and_keeps_an_atomic_incomplete_status_and_releases_lock(self):
        os.makedirs(os.path.join(self.config['root'], 'results', '.staging-abandoned'))
        original = shutil.rmtree
        entered = []
        def stalled(path, *args, **kwargs):
            if str(path).endswith('.staging-abandoned'):
                entered.append(True)
                time.sleep(30)
            return original(path, *args, **kwargs)
        started = time.monotonic()
        # First cycle establishes a failure record before the next one's stale sweep.
        self.bounded_cycle({})
        os.makedirs(os.path.join(self.config['root'], 'results', '.staging-abandoned'), exist_ok=True)
        with mock.patch.object(shutil, 'rmtree', side_effect=stalled):
            doc = self.bounded_cycle({})
        self.assertTrue(entered)
        self.assertLess(time.monotonic() - started, 2.5)
        self.assertEqual(doc['status'], 'failed')
        # __enter__ was interrupted: close its fd even though Python won't call __exit__.
        with telemetrylab.HistoryRoot(self.config['root']):
            pass
