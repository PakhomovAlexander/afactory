"""Corpus preparation is deterministic and serial preparation never needs IPC."""
import contextlib
import io
import json
from pathlib import Path
import subprocess
import sys
from types import SimpleNamespace
from unittest import mock

from perflab import telemetrylab
from tests.support import ROOT, TempDir


class CorpusPreparation(TempDir):
    def test_serial_never_constructs_a_pool_and_matches_parallel_bytes_and_facts(self):
        module = telemetrylab.script_module('scripts/telemetry-corpus.py')
        serial, parallel = Path(self.path('serial')), Path(self.path('parallel'))
        captured = io.StringIO()
        # The exact failure class from SemLock under --ipc none. Even entering the
        # multiprocessing context fails this regression, including a one-worker Pool.
        with mock.patch.object(module.multiprocessing, 'get_context',
                               side_effect=FileNotFoundError('no IPC semaphores')) as ipc, \
                mock.patch('multiprocessing.pool.Pool',
                           side_effect=AssertionError('serial created a Pool')) as pool, \
                mock.patch('multiprocessing.synchronize._multiprocessing.SemLock',
                           side_effect=FileNotFoundError('no IPC semaphores')) as semlock, \
                contextlib.redirect_stdout(captured):
            self.assertEqual(module.generate(SimpleNamespace(
                out=str(serial), seed=20261021, runs=2, jobs=1)), 0)
        ipc.assert_not_called()
        pool.assert_not_called()
        semlock.assert_not_called()
        done = subprocess.run([sys.executable, '-I', '-B',
            str(Path(ROOT, 'scripts/telemetry-corpus.py')), 'generate', '--out', str(parallel),
            '--seed', '20261021', '--runs', '2', '--jobs', '2'],
            capture_output=True, timeout=60)
        self.assertEqual(done.returncode, 0, done.stderr)
        # Only the location and elapsed preparation time can differ. All generated facts
        # and every retained byte (gzip headers included) must match.
        summaries = [json.loads(captured.getvalue()), json.loads(done.stdout)]
        for summary in summaries:
            del summary['out'], summary['seconds']
        self.assertEqual(*summaries)
        self.assertEqual(telemetrylab.corpus_facts(str(serial)),
                         telemetrylab.corpus_facts(str(parallel)))
        files = lambda root: {str(p.relative_to(root)): p.read_bytes()
                              for p in root.rglob('*') if p.is_file()}
        self.assertEqual(files(serial), files(parallel))
        self.assertEqual(summaries[0]['executions'], 4)
        self.assertEqual(summaries[0]['blobs'], 40)

    def test_contained_preparation_explicitly_selects_serial_jobs(self):
        registry, _ = telemetrylab.load_registry()
        for profile in registry['profiles']:
            config = telemetrylab.configuration(registry, profile)
            argv = telemetrylab.corpus_argv(config['read_latency'], config['seed'], 'script')
            self.assertEqual(argv[argv.index('--jobs') + 1], '1')
            # Gross capacity preserves the former workload allowance above the real
            # production floor, with another 2 GiB for filesystem overhead/headroom.
            workload = {'pr': 4, 'nightly': 8, 'release': 12, 'manual': 8}[profile] << 30
            self.assertEqual(config['work_bytes'], workload + (12 << 30))
            self.assertLessEqual(config['work_bytes'], 64 << 30)
