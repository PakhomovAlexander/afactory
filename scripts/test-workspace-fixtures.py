#!/usr/bin/env python3
"""Selected interpreter/isolation and fail-closed paired benchmark evidence contracts."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts' / filename)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


W = module('workspace_tests', 'test-scenario-git-workspace.py')
B = module('workspace_benchmark', 'benchmark-workspace-fixtures.py')


def sample_report():
    counts = dict(schema='af/workspace-fixture-suite@1', expected=2, tests=2, successful=True,
                  failures=0, errors=0, skipped=0, expected_failures=0, unexpected_successes=0)
    runs = [dict(pair=p, side=side, wall_seconds=float(p + (side == 'head')), exit_code=0,
                 timed_out=False, counts=copy.deepcopy(counts), log=f'{p}-{side}.log', receipt=f'{p}-{side}.receipt.json')
            for p in range(1, 4) for side in (('base', 'head') if p % 2 else ('head', 'base'))]
    errors, comparison = B.evaluate(runs, 3)
    identity = dict(commit='a' * 40, git_tree='b' * 40, export_sha256='c' * 64, suite_sha256='d' * 64)
    return dict(schema=B.KIND, suite=B.SUITE, pairs=3, platform='test',
                interpreter=dict(path='/selected/python', version='test', sha256='e' * 64),
                sources=dict(base=identity, head=identity), runs=runs,
                valid=True, errors=errors, comparison=comparison)


class FixtureInterpreterTests(unittest.TestCase):
    def test_platform_launchers_execute_isolated_despite_hostile_environment(self):
        canonical = str(Path(sys.executable).resolve())
        for platform, expected in (('darwin', canonical), ('linux', '/usr/bin/python3')):
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                (root / 'sitecustomize.py').write_text("raise RuntimeError('ambient site imported')\n")
                (root / 'json.py').write_text("raise RuntimeError('ambient import path used')\n")
                # Both a PATH lookup and an accidental selected-interpreter launch must fail.
                hostile = root / 'python3'
                hostile.write_text("#!/bin/sh\nexit 97\n")
                hostile.chmod(0o700)
                alias = root / 'python-alias'
                alias.symlink_to(canonical)
                selected = str(alias) if platform == 'darwin' else str(hostile)
                launcher = root / 'fixture with spaces'
                with patch.object(W.sys, 'platform', platform), patch.object(W.sys, 'executable', selected):
                    launcher.write_text(W.fixture_launcher(
                        "import json,sys\nprint(json.dumps([sys.executable,sys.flags.isolated,sys.flags.no_site,'site' in sys.modules,sys.argv[1:]]))\n"))
                launcher.chmod(0o700)
                self.assertEqual(launcher.read_text().splitlines()[0], '#!' + expected + ' -IS')
                env = dict(PATH=str(root), HOME=str(root), PYTHONPATH=str(root), PYTHONHOME=str(root))
                result = subprocess.run([str(launcher), 'argument with spaces'], cwd=root, env=env,
                                        capture_output=True, text=True, check=True)
                # Apple's /usr/bin/python3 dispatches to CommandLineTools; compare its real
                # identity while still requiring the literal system path in the shebang.
                identity = subprocess.check_output([expected, '-IS', '-c', 'import sys; print(sys.executable)'],
                                                   cwd=root, env=env, text=True).strip()
                self.assertEqual(json.loads(result.stdout), [identity, 1, 1, False, ['argument with spaces']])

    def test_non_darwin_ignores_unused_selected_python_even_when_invalid(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            spaced = root / 'Python Framework'
            spaced.touch()
            long_path = root / ('p' * 128)
            long_path.touch()
            for platform in ('linux', 'freebsd'):
                for selected in (sys.executable, str(root / 'missing'), str(spaced), str(long_path), ''):
                    with self.subTest(platform=platform, selected=selected), \
                            patch.object(W.sys, 'platform', platform), patch.object(W.sys, 'executable', selected):
                        self.assertEqual(W.fixture_launcher('pass\n'), '#!/usr/bin/python3 -IS\npass\n')

    def test_darwin_missing_interpreter_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            with patch.object(W.sys, 'platform', 'darwin'), \
                    patch.object(W.sys, 'executable', str(Path(tmp) / 'missing')):
                with self.assertRaises(FileNotFoundError):
                    W.fixture_launcher('pass\n')

    def test_interpreter_path_with_whitespace_is_refused_explicitly(self):
        with tempfile.TemporaryDirectory() as tmp:
            python = Path(tmp) / 'Python Framework'
            python.touch()
            with patch.object(W.sys, 'platform', 'darwin'), patch.object(W.sys, 'executable', str(python)):
                with self.assertRaisesRegex(ValueError, 'Python path.*no whitespace'):
                    W.fixture_launcher('pass\n')

    def test_portably_unrepresentable_shebang_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            python = Path(tmp) / ('p' * 128)
            python.touch()
            with patch.object(W.sys, 'platform', 'darwin'), patch.object(W.sys, 'executable', str(python)):
                with self.assertRaisesRegex(ValueError, '127-byte limit'):
                    W.fixture_launcher('pass\n')

    def test_each_generated_fixture_uses_launcher_and_production_installer_keeps_policy(self):
        source = (ROOT / 'scripts/test-scenario-git-workspace.py').read_text()
        for name in ('af', 'lab', 'wrapper', 'controlled'):
            self.assertIn(name + '.write_text(fixture_launcher(', source)
        self.assertTrue(W.I.source().startswith('#!/usr/bin/python3 -IS\n'))


class EvidenceTests(unittest.TestCase):
    def test_versioned_schema_parity_roundtrip_and_median_calculation(self):
        schema = json.loads((ROOT / 'schemas/workspace-fixture-benchmark-v1.json').read_text())
        self.assertEqual(schema, B.report_schema())
        suite_schema = json.loads((ROOT / 'schemas/workspace-fixture-suite-v1.json').read_text())
        self.assertEqual(suite_schema, dict(B.suite_schema(), **{
            '$schema': 'https://json-schema.org/draft/2020-12/schema',
            '$id': 'urn:af:schema:workspace-fixture-suite:1'}))
        report = sample_report()
        B.check_shape(report, schema)
        B.check_shape(report['runs'][0]['counts'], suite_schema)
        self.assertEqual(report['comparison']['base_median_seconds'], 2)
        self.assertEqual(report['comparison']['head_median_seconds'], 3)
        self.assertEqual(report['comparison']['paired_head_minus_base_median_seconds'], 1)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'report.json'
            path.write_text(json.dumps(report))
            self.assertEqual(B.read_report(path), report)
            for change in ('version', 'comparison', 'validity', 'extra', 'incomplete', 'type'):
                bad = copy.deepcopy(report)
                if change == 'version':
                    bad['schema'] = 'af/workspace-fixture-benchmark@0'
                elif change == 'comparison':
                    bad['comparison']['head_median_seconds'] = 0
                elif change == 'validity':
                    bad['valid'] = False
                elif change == 'extra':
                    bad['runs'][0]['extra'] = True
                elif change == 'incomplete':
                    bad['runs'].pop()
                else:
                    bad['runs'][0]['counts']['tests'] = True
                path.write_text(json.dumps(bad))
                with self.subTest(change=change), self.assertRaises(ValueError):
                    B.read_report(path)

    def test_malformed_or_retired_receipts_are_retained_but_not_admitted(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp)
            receipt = output / '1-head.receipt.json'
            for content in ('{broken', '{}', '{"schema":"af/workspace-fixture-suite@0"}'):
                receipt.write_text(content)
                with patch.object(B.subprocess, 'Popen') as spawn:
                    spawn.return_value.__enter__.return_value.wait.return_value = 0
                    run = B.run_suite(output, output, 'head', 1, 600)
                self.assertIsNone(run['counts'])
                self.assertEqual(receipt.read_text(), content)

    def test_refuses_failure_missing_receipts_unequal_counts_and_incomplete_pairs(self):
        for change in ('exit', 'timeout', 'missing', 'failure', 'skip', 'partial', 'unequal', 'order', 'missing_run', 'receipt_version'):
            runs = sample_report()['runs']
            if change == 'exit':
                runs[0]['exit_code'] = 1
            elif change == 'timeout':
                runs[0]['timed_out'] = True
            elif change == 'missing':
                runs[0]['counts'] = None
            elif change in ('failure', 'skip'):
                runs[0]['counts']['failures' if change == 'failure' else 'skipped'] = 1
            elif change == 'partial':
                runs[0]['counts']['tests'] = 1
            elif change == 'unequal':
                runs[0]['counts'].update(tests=3, expected=3)
            elif change == 'order':
                runs.reverse()
            elif change == 'receipt_version':
                runs[0]['counts']['schema'] = 'af/workspace-fixture-suite@0'
            else:
                runs.pop()
            with self.subTest(change=change):
                errors, comparison = B.evaluate(runs, 3)
                self.assertTrue(errors)
                self.assertIsNone(comparison)


if __name__ == '__main__':
    unittest.main()
