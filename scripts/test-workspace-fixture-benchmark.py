#!/usr/bin/env python3
"""Small real-process benchmark integration tests; synthetic suites prove evidence, not speed."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import MagicMock, patch

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location('workspace_benchmark', ROOT / 'scripts/benchmark-workspace-fixtures.py')
B = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(B)


class BenchmarkTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.repo = self.root / 'repo with spaces'
        self.repo.mkdir()
        home = self.root / 'home'
        home.mkdir()
        self.env = dict(PATH=os.defpath, HOME=str(home), GIT_CONFIG_NOSYSTEM='1',
                        GIT_CONFIG_GLOBAL=os.devnull, GIT_AUTHOR_NAME='Test',
                        GIT_AUTHOR_EMAIL='test@example.invalid', GIT_COMMITTER_NAME='Test',
                        GIT_COMMITTER_EMAIL='test@example.invalid')
        self.git('init', '-q')
        (self.repo / 'scripts').mkdir()
        self.script = self.repo / B.SUITE
        self.write_suite()
        self.git('add', '.')
        self.git('commit', '-qm', 'base fixture')
        self.base = self.git('rev-parse', 'HEAD')

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.repo), *args], env=self.env).decode().strip()

    def write_suite(self, body='self.assertTrue(True)', extra=''):
        self.script.write_text('import unittest\nclass Complete(unittest.TestCase):\n'
            ' def test_first(self):\n  ' + body + '\n'
            ' def test_second(self):\n  self.assertTrue(True)\n' + extra +
            "if __name__ == '__main__': unittest.main()\n")

    def run_benchmark(self):
        self.git('add', '.')
        self.git('commit', '--allow-empty', '-qm', 'head fixture')
        head = self.git('rev-parse', 'HEAD')
        # Dirty checkout bytes must never become the measured head, even its suite.
        self.script.write_text("raise RuntimeError('dirty working tree executed')\n")
        output = self.root / 'evidence'
        result = subprocess.run([sys.executable, '-I', '-S', '-B', str(ROOT / 'scripts/benchmark-workspace-fixtures.py'),
            '--repo', str(self.repo), '--base', self.base, '--head', head, '--output', str(output),
            '--summary', str(self.root / 'job-summary.md')], env=self.env, capture_output=True, text=True)
        self.assertTrue((output / 'benchmark.json').exists(), result.stderr)
        report = B.read_report(output / 'benchmark.json')
        self.assertEqual([(r['pair'], r['side']) for r in report['runs']],
                         [(1, 'base'), (1, 'head'), (2, 'head'), (2, 'base'), (3, 'base'), (3, 'head')])
        for run in report['runs']:
            self.assertTrue((output / run['log']).read_text())
            self.assertEqual(json.loads((output / run['receipt']).read_text()), run['counts'])
            self.assertGreater(run['wall_seconds'], 0)
        self.assertEqual(report['sources']['base']['commit'], self.base)
        self.assertEqual(report['sources']['head']['commit'], head)
        self.assertEqual(report['interpreter']['path'], str(Path(sys.executable).resolve()))
        self.assertEqual((self.root / 'job-summary.md').read_text(), (output / 'summary.md').read_text())
        self.assertFalse((self.repo / 'scripts/benchmark-workspace-fixtures.py').exists())
        return result, report

    def test_complete_suites_at_pinned_commits_need_no_helper_on_old_base(self):
        result, report = self.run_benchmark()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(report['valid'])
        self.assertEqual([r['counts']['tests'] for r in report['runs']], [2] * 6)
        self.assertEqual([r['exit_code'] for r in report['runs']], [0] * 6)
        self.assertIsNotNone(report['comparison'])

    def test_failing_complete_head_retains_all_six_runs_and_refuses_comparison(self):
        self.write_suite('self.fail("fixture failure")')
        result, report = self.run_benchmark()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertFalse(report['valid'])
        self.assertIsNone(report['comparison'])
        self.assertEqual([r['exit_code'] for r in report['runs'] if r['side'] == 'head'], [1] * 3)
        self.assertEqual([r['counts']['tests'] for r in report['runs']], [2] * 6)

    def test_unequal_complete_counts_refuse_comparison(self):
        self.write_suite(extra=' def test_extra(self):\n  self.assertTrue(True)\n')
        result, report = self.run_benchmark()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn('unequal or absent test counts', report['errors'])
        self.assertIsNone(report['comparison'])

    def test_early_successful_exit_is_incomplete_evidence(self):
        self.script.write_text('raise SystemExit(0)\n')
        self.git('add', '.')
        self.git('commit', '-qm', 'incomplete fixture')
        export = self.root / 'export'
        B.export(self.repo, self.git('rev-parse', 'HEAD'), export)
        run = B.run_suite(export, self.root, 'head', 1, 600)
        self.assertEqual(run['exit_code'], 0)
        self.assertIsNone(run['counts'])
        errors, comparison = B.evaluate([run], 3)
        self.assertTrue(errors)
        self.assertIsNone(comparison)

    def test_export_refuses_symlink_escape_and_frozen_export_restores_cleanup_access(self):
        (self.repo / 'escape').symlink_to('../outside')
        self.git('add', '.')
        self.git('commit', '-qm', 'unsafe fixture')
        with self.assertRaisesRegex(ValueError, 'symlink escapes'):
            B.export(self.repo, self.git('rev-parse', 'HEAD'), self.root / 'unsafe')
        export = self.root / 'safe'
        B.export(self.repo, self.base, export)
        before = B.tree_digest(export)
        with B.frozen([export]):
            self.assertEqual((export / B.SUITE).stat().st_mode & 0o222, 0)
            self.assertEqual(export.stat().st_mode & 0o222, 0)
        self.assertEqual(B.tree_digest(export), before)

    def test_timeout_records_failure_without_waiting_for_a_real_deadline(self):
        for race in (None, ProcessLookupError()):
            process = MagicMock()
            process.__enter__.return_value = process
            process.pid = 12345
            process.wait.side_effect = [subprocess.TimeoutExpired('fixture', 600), -9]
            with patch.object(B.subprocess, 'Popen', return_value=process) as spawn, \
                 patch.object(B.os, 'killpg', side_effect=race) as kill:
                run = B.run_suite(self.repo, self.root, 'head', 1, 600)
            self.assertTrue(spawn.call_args.kwargs['start_new_session'])
            kill.assert_called_once_with(process.pid, B.signal.SIGKILL)
            self.assertEqual(process.wait.call_count, 2)
            self.assertTrue(run['timed_out'])
            self.assertIsNone(run['exit_code'])
            self.assertIsNone(run['counts'])



if __name__ == '__main__':
    unittest.main()
