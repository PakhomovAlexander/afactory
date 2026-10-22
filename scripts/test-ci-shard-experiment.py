#!/usr/bin/env python3
"""Focused, standard-library checks for the measurement-only shard experiment."""

import ast
from contextlib import redirect_stdout
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
HELPER = ROOT / 'scripts/ci-shard-experiment.py'
WORKFLOW = ROOT / '.github/workflows/ci-shard-experiment.yml'
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('ci_shard_experiment', HELPER)
experiment = importlib.util.module_from_spec(spec)
spec.loader.exec_module(experiment)


def manifest(runnable, ignored=(), filtered=()):
    cases = {name: {'ignored': False, 'filter-match': {'status': 'matches'}}
             for name in runnable}
    cases.update({name: {'ignored': True, 'filter-match': {'status': 'matches'}}
                  for name in ignored})
    cases.update({name: {'ignored': False, 'filter-match': {'status': 'mismatch'}}
                  for name in filtered})
    return {'rust-suites': {'suite': {'binary-id': 'fixture-bin', 'testcases': cases}}}


class ManifestChecks(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)

    def verify(self, full, first, second):
        for name, content in (('full', full), ('first', first), ('second', second)):
            (self.base / f'{name}.json').write_text(json.dumps(content))
        args = mock.Mock(full=self.base / 'full.json', first=self.base / 'first.json',
                         second=self.base / 'second.json')
        with redirect_stdout(io.StringIO()):
            experiment.verify(args)

    def test_complete_disjoint_partition_and_filtered_cases(self):
        self.verify(manifest(['a', 'b'], ['ignored'], ['filtered']),
                    manifest(['a'], ['ignored'], ['b', 'filtered']),
                    manifest(['b'], ['ignored'], ['a', 'filtered']))

    def test_overlap_fails(self):
        with self.assertRaisesRegex(SystemExit, 'overlap'):
            self.verify(manifest(['a', 'b']), manifest(['a']), manifest(['a', 'b']))

    def test_omission_fails(self):
        with self.assertRaisesRegex(SystemExit, 'union differs'):
            self.verify(manifest(['a', 'b', 'c']), manifest(['a']), manifest(['b']))

    def test_ignored_inventory_mismatch_fails(self):
        with self.assertRaisesRegex(SystemExit, 'ignored test inventory'):
            self.verify(manifest(['a', 'b'], ['ignored']),
                        manifest(['a'], ['ignored']), manifest(['b']))

    def test_empty_shard_and_empty_full_fail(self):
        with self.assertRaisesRegex(SystemExit, 'empty'):
            self.verify(manifest(['a']), manifest(['a']), manifest([]))
        with self.assertRaisesRegex(SystemExit, 'empty'):
            self.verify(manifest([]), manifest(['a']), manifest(['b']))

    def test_record_uses_ci_profile_and_rejects_default_filter_zero_selection(self):
        out = self.base / 'selection.json'
        args = mock.Mock(archive=None, extract_to=None, partition='count:1/2', output=out)
        zero = manifest([], filtered=['excluded-by-ci-default-filter'])
        with mock.patch.object(experiment.subprocess, 'run', return_value=mock.Mock(
            returncode=0, stdout=json.dumps(zero), stderr='')) as run:
            with self.assertRaisesRegex(SystemExit, 'empty nextest selection'):
                experiment.record(args)
            command = run.call_args.args[0]
        self.assertEqual(command[:4], ['cargo', 'nextest', 'list', '--message-format'])
        self.assertEqual(command[command.index('--profile') + 1], 'ci')
        self.assertEqual(command[command.index('--partition') + 1], 'count:1/2')
        self.assertIn('--locked', command)
        self.assertFalse(out.exists())

    def test_malformed_manifest_fails_closed(self):
        (self.base / 'full.json').write_text('{invalid json')
        (self.base / 'first.json').write_text(json.dumps(manifest(['a'])))
        (self.base / 'second.json').write_text(json.dumps(manifest(['b'])))
        args = mock.Mock(full=self.base / 'full.json', first=self.base / 'first.json',
                         second=self.base / 'second.json')
        with self.assertRaises(json.JSONDecodeError):
            experiment.verify(args)
        args = mock.Mock(archive=None, extract_to=None, partition=None,
                         output=self.base / 'out.json')
        with mock.patch.object(experiment.subprocess, 'run', return_value=mock.Mock(
            returncode=0, stdout='{invalid json', stderr='')):
            with self.assertRaises(json.JSONDecodeError):
                experiment.record(args)
        self.assertFalse(args.output.exists())

    def test_record_archive_profile_and_malformed_list_fail(self):
        args = mock.Mock(archive=self.base / 'tests.tar.zst', extract_to=self.base,
                         partition=None, output=self.base / 'selection.json')
        with mock.patch.object(experiment.subprocess, 'run', return_value=mock.Mock(
            returncode=0, stdout=json.dumps(manifest(['a'])), stderr='')) as run:
            with redirect_stdout(io.StringIO()):
                experiment.record(args)
            command = run.call_args.args[0]
        self.assertIn('--archive-file', command)
        self.assertIn('--extract-overwrite', command)
        self.assertNotIn('--locked', command)
        self.assertEqual(command[command.index('--profile') + 1], 'ci')
        with mock.patch.object(experiment.subprocess, 'run', return_value=mock.Mock(
            returncode=1, stdout='', stderr='bad list')):
            with self.assertRaisesRegex(SystemExit, 'nextest list failed'):
                experiment.record(args)


class CacheChecks(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.fake = self.base / 'sccache'
        self.fake.write_text('''#!/usr/bin/env python3
import os, sys
from pathlib import Path
with Path(os.environ['FAKE_LOG']).open('a') as out:
    out.write('sccache ' + ' '.join(sys.argv[1:]) + '\\n')
print(os.environ['FAKE_STATS'])
''')
        self.fake.chmod(0o755)
        self.out = self.base / 'single-compile-cache.json'
        self.log = self.base / 'order.log'

    def invoke(self, stats):
        env = dict(os.environ, PATH=f'{self.base}:{os.environ.get("PATH", "")}',
                   FAKE_STATS=stats, FAKE_LOG=str(self.log))
        return subprocess.run([sys.executable, str(HELPER), 'cache-stats',
                               '--arm', 'single', '--output', str(self.out)],
                              env=env, capture_output=True, text=True)

    def test_snapshot_precedes_long_test_and_keeps_raw_counters(self):
        raw = {'stats': {'compile_requests': 12, 'cache_hits': {'counts': {'Rust': 7}},
                         'cache_misses': {'counts': {'Rust': 3}}}, 'cache_location': 'fixture'}
        result = self.invoke(json.dumps(raw))
        self.assertEqual(result.returncode, 0, result.stderr)
        with self.log.open('a') as out:
            out.write('long-test started\n')
        self.assertEqual(self.log.read_text().splitlines(),
                         ['sccache --show-stats --stats-format json', 'long-test started'])
        data = json.loads(self.out.read_text())
        self.assertEqual((data['arm'], data['phase'], data['compile_requests'],
                          data['cache_hits'], data['cache_misses']),
                         ('single', 'after-compile-before-test', 12, 7, 3))
        self.assertEqual(data['sccache'], raw)

    def test_missing_or_zero_statistics_fail_without_artifact(self):
        for raw in ({}, {'stats': {}}, {'stats': {'compile_requests': 0,
                    'cache_hits': {'counts': {}}, 'cache_misses': {'counts': {}}}},
                    {'stats': {'compile_requests': 2, 'cache_hits': {'counts': {}},
                     'cache_misses': {'counts': {}}}},
                    {'stats': {'compile_requests': 2, 'cache_hits': {'counts': {'Rust': 3}},
                     'cache_misses': {'counts': {}}}}):
            with self.subTest(raw=raw):
                self.assertNotEqual(self.invoke(json.dumps(raw)).returncode, 0)
                self.assertFalse(self.out.exists())
        self.assertNotEqual(self.invoke('not json').returncode, 0)
        self.assertFalse(self.out.exists())

    def test_workflow_shell_collects_cache_before_long_test(self):
        cargo = self.base / 'cargo'
        cargo.write_text('''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
with Path(os.environ['FAKE_LOG']).open('a') as out:
    out.write('cargo ' + ' '.join(args) + '\\n')
if args[:2] == ['nextest', 'list']:
    names = ['a', 'b']
    if '--partition' in args:
        names = ['a'] if args[args.index('--partition') + 1] == 'count:1/2' else ['b']
    print(json.dumps({'rust-suites': {'suite': {'binary-id': 'fixture-bin',
        'testcases': {n: {'ignored': False, 'filter-match': {'status': 'matches'}}
                      for n in names}}}}))
''')
        cargo.chmod(0o755)
        body = re.search(r'^  single:\n(.*?)(?=^  archive-build:)',
                         WORKFLOW.read_text(), re.M | re.S).group(1)
        block = body.split('      - name: Compile and run unsharded selection', 1)[1]
        block = block.split('        run: |\n', 1)[1].split('      - name:', 1)[0]
        commands = '\n'.join(line[10:] for line in block.splitlines() if line.strip())
        env = dict(os.environ, PATH=f'{self.base}:{os.environ.get("PATH", "")}',
                   FAKE_LOG=str(self.log), RUNNER_TEMP=str(self.base),
                   AF_CI_METRICS=str(self.base / 'metrics.jsonl'),
                   FAKE_STATS=json.dumps({'stats': {'compile_requests': 2,
                       'cache_hits': {'counts': {'Rust': 1}},
                       'cache_misses': {'counts': {'Rust': 1}}}}))
        result = subprocess.run(['bash', '-e', '-c', commands], cwd=ROOT, env=env,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        lines = self.log.read_text().splitlines()
        self.assertLess(next(i for i, line in enumerate(lines) if line.startswith('cargo test ')),
                        next(i for i, line in enumerate(lines) if line.startswith('sccache ')))
        self.assertLess(next(i for i, line in enumerate(lines) if line.startswith('sccache ')),
                        next(i for i, line in enumerate(lines) if line.startswith('cargo nextest run ')))
        self.log.unlink()
        env['FAKE_STATS'] = '{}'
        failed = subprocess.run(['bash', '-e', '-c', commands], cwd=ROOT, env=env,
                                capture_output=True, text=True)
        self.assertNotEqual(failed.returncode, 0)
        self.assertFalse(any(line.startswith('cargo nextest run ')
                             for line in self.log.read_text().splitlines()))

    def test_zero_hits_with_real_misses_is_valid(self):
        raw = {'stats': {'compile_requests': 4, 'cache_hits': {'counts': {}},
                         'cache_misses': {'counts': {'Rust': 4}}}}
        self.assertEqual(self.invoke(json.dumps(raw)).returncode, 0)
        self.assertEqual(json.loads(self.out.read_text())['cache_hits'], 0)


class WorkflowChecks(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = WORKFLOW.read_text()
        cls.jobs = dict(re.findall(r'^  ([a-z][a-z-]+):\n(.*?)(?=^  [a-z][a-z-]+:\n|\Z)',
                                   cls.workflow, re.M | re.S))

    def test_python_syntax(self):
        for path in (HELPER, Path(__file__), ROOT / 'scripts/ci-step.py'):
            with self.subTest(path=path):
                ast.parse(path.read_text(), filename=str(path))

    def test_compile_snapshot_order_in_every_compiling_arm(self):
        for job, build, arm in (('single', 'test-build cargo test', 'single'),
                                ('archive-build', 'archive-build cargo nextest archive',
                                 'archive-build'),
                                ('rebuild-shard', 'test-build cargo test',
                                 'rebuild-shard-${{ matrix.shard }}')):
            with self.subTest(job=job):
                body = self.jobs[job]
                self.assertLess(body.index(build), body.index('cache-stats --arm ' + arm))
                self.assertLess(body.index('cache-stats --arm ' + arm),
                                body.index('manifest-proof'))
                self.assertIn(f'{arm}-compile-cache.json', body)
                self.assertIn('${{ runner.temp }}/cache-metrics.json', body)
                if job != 'archive-build':
                    self.assertLess(body.index('cache-stats --arm ' + arm),
                                    body.index('ci-step.py test cargo nextest run'))
                else:
                    self.assertLess(body.index('cache-stats --arm ' + arm),
                                    body.index('Share checked archive'))

    def test_security_and_exact_source(self):
        text = self.workflow
        self.assertRegex(text, r'(?m)^  pull_request:\n    paths:')
        trigger = text.split('\non:\n', 1)[1].split('\npermissions:', 1)[0]
        self.assertEqual([line.strip() for line in trigger.splitlines() if line.strip()], [
            'pull_request:', 'paths:',
            '- .github/workflows/ci-shard-experiment.yml',
            '- scripts/ci-shard-experiment.py',
            '- docs/ci-optimization.md', 'workflow_dispatch:'])
        self.assertNotIn('pull_request_target', text)
        self.assertRegex(text, r'(?m)^permissions:\n  contents: read$')
        self.assertIn('github.event.pull_request.head.sha || github.sha', text)
        self.assertEqual(text.count('persist-credentials: false'), 5)
        self.assertEqual(text.count('ref: ${{ env.AF_EXPERIMENT_HEAD_SHA }}'), 5)
        self.assertEqual(text.count('test "$(git rev-parse HEAD)" = "$AF_EXPERIMENT_HEAD_SHA"'), 5)
        for action in re.findall(r'(?m)^\s+- uses: (.+)$', text):
            if action.startswith('./'):
                continue
            self.assertRegex(action, r'@[0-9a-f]{40}(?: # .*)?$')
        for job in ('single', 'archive-build', 'rebuild-shard'):
            self.assertIn('head.repo.full_name == github.repository', self.jobs[job])
        self.assertIn('needs: [single, archive-build, archive-shard, rebuild-shard]',
                      self.jobs['aggregate'])
        self.assertIn('fail-fast: false', self.jobs['archive-shard'])
        self.assertIn('fail-fast: false', self.jobs['rebuild-shard'])
        self.assertEqual(text.count('--no-tests fail'), 3)


if __name__ == '__main__':
    unittest.main()
