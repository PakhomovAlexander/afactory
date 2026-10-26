#!/usr/bin/env python3
"""Selection, inventory, release binding, aggregation and evidence contracts, offline."""
import copy
import importlib.util
import itertools
import json
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('testclasses', ROOT / 'scripts/test-classes.py')
tc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tc)


class Selection(unittest.TestCase):
    def test_defaults_and_profiles(self):
        self.assertEqual(tc.resolve(context='pr')['selected'], tc.FAST)
        self.assertEqual(tc.resolve(context='release')['selected'], list(tc.CLASSES))
        self.assertEqual(tc.resolve()['selected'], list(tc.CLASSES))
        self.assertEqual(tc.resolve(context='gate')['selected'], list(tc.CLASSES))
        self.assertEqual(tc.event_context({'pull_request': {'base': {'ref': 'af-gate/task/base'}}}), 'gate')
        self.assertEqual(tc.event_context({'pull_request': {'base': {'ref': 'main'}}}), 'pr')
        self.assertEqual(tc.event_context({'pull_request': {'head': {'ref': 'release/v1.2.3'}}}), 'release')
        self.assertEqual(tc.resolve(['release:fast'], 'release')['selected'], tc.FAST)
        self.assertEqual(tc.resolve(['test:functional', 'test:e2e'], 'pr')['selected'], ['functional', 'e2e'])
        self.assertEqual(tc.resolve(['test:perf'], 'pr')['selected'], ['perf'])
        self.assertEqual(tc.resolve(['no-perf'], 'pr')['selected'], tc.FAST)
        self.assertEqual(tc.resolve(['release:full'], 'pr')['selected'], list(tc.CLASSES))

    def test_exhaustive_allowlists_and_exclusions(self):
        for flags in itertools.product((0, 1, 2), repeat=4):
            labels = [f'test:{c}' if f == 1 else f'no-{c}' for c, f in zip(tc.CLASSES, flags) if f]
            positive = {c for c, f in zip(tc.CLASSES, flags) if f == 1}
            negative = {c for c, f in zip(tc.CLASSES, flags) if f == 2}
            for context in ('pr', 'release', 'local'):
                expected = (positive or set(tc.FAST if context == 'pr' else tc.CLASSES)) - negative
                with self.subTest(labels=labels, context=context):
                    if expected:
                        self.assertEqual(set(tc.resolve(labels, context)['selected']), expected)
                    else:
                        with self.assertRaises(ValueError):
                            tc.resolve(labels, context)

    def test_conflicts_unknown_and_full_refusals(self):
        for labels in [['test:foo'], ['no-foo'], ['release:foo'], ['release:fast', 'release:full'],
                       *[[f'test:{c}', f'no-{c}'] for c in tc.CLASSES],
                       *[['release:full', f'no-{c}'] for c in tc.CLASSES],
                       ['release:full', 'test:functional']]:
            with self.subTest(labels=labels), self.assertRaises(ValueError):
                tc.resolve(labels)

    def test_event_data_is_not_shell(self):
        label = 'harmless; $(touch SHOULD_NOT_EXIST)\noutput=oops'
        self.assertEqual(tc.resolve([label], 'pr')['selected'], tc.FAST)
        with self.assertRaises(ValueError):
            tc.resolve(['test:perf\nselected=[]'])
        with tempfile.TemporaryDirectory() as tmp:
            event = Path(tmp) / 'event.json'
            output = Path(tmp) / 'plan.json'
            gh = Path(tmp) / 'github-output'
            event.write_text(json.dumps({'pull_request': {'labels': [{'name': label}]}}))
            subprocess.run([sys.executable, str(ROOT / 'scripts/test-classes.py'), 'plan',
                            '--context', 'pr', '--event', str(event), '--output', str(output),
                            '--github-output', str(gh)], check=True, capture_output=True)
            self.assertEqual(len(gh.read_text().splitlines()), 3)
            self.assertFalse((ROOT / 'SHOULD_NOT_EXIST').exists())


class Release(unittest.TestCase):
    sha = 'a' * 40

    def binding(self, labels=('release:fast',)):
        return {'schema': tc.BINDING, 'sha': self.sha, 'version': '1.2.3',
                'source_pr': {'repository': 'owner/repo', 'number': 335,
                              'merge_commit_sha': self.sha, 'base': 'main',
                              'head': 'release/v1.2.3', 'merged_at': '2026-10-25T11:36:48Z'},
                'plan': tc.resolve(labels, 'release', self.sha, '1.2.3')}

    def test_captured_tag_rerun_is_immutable(self):
        binding = self.binding()
        self.assertEqual(tc.release_plan(json.dumps(binding), self.sha, '1.2.3')['selected'], tc.FAST)
        binding['source_pr'] = None
        with self.assertRaises(ValueError):
            tc.release_plan(json.dumps(binding), self.sha, '1.2.3')

    def test_direct_manual_tags_default_full(self):
        for annotation in ('', 'v1.2.3', 'manual release'):
            self.assertEqual(tc.release_plan(annotation, self.sha, '1.2.3')['selected'], list(tc.CLASSES))

    def test_stale_annotation_and_tampered_plan_fail_closed(self):
        binding = self.binding()
        for sha, version in [('b' * 40, '1.2.3'), (self.sha, '1.2.4')]:
            with self.assertRaises(ValueError):
                tc.release_plan(json.dumps(binding), sha, version)
        for key, value in [('base', 'other'), ('head', 'release/v1.2.4'), ('merged_at', ''),
                           ('merge_commit_sha', 'b' * 40), ('number', True)]:
            bad = copy.deepcopy(binding)
            bad['source_pr'][key] = value
            with self.assertRaises(ValueError):
                tc.release_plan(json.dumps(bad), self.sha, '1.2.3')
        binding['plan']['selected'] = []
        with self.assertRaises(ValueError):
            tc.release_plan(json.dumps(binding), self.sha, '1.2.3')
        with self.assertRaises(ValueError):
            tc.release_plan(tc.BINDING, self.sha, '1.2.3')

    def test_release_labels_change_without_source_mutation(self):
        from unittest import mock
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Path(tmp)
            manifest = fixture / 'Cargo.toml'
            manifest.write_text('[workspace.package]\nversion = "1.2.3"\n')
            before = manifest.read_bytes()
            event = {'pull_request': {'head': {'ref': 'release/v1.2.3'}}}
            with mock.patch.object(tc, 'ROOT', fixture):
                for labels in (['release:fast'], ['release:full'], ['test:functional', 'test:e2e']):
                    tc.check_release_pr(event)
                    self.assertTrue(tc.resolve(labels, 'release')['selected'])
                event['pull_request']['head']['ref'] = 'release/v1.2.4'
                with self.assertRaises(ValueError):
                    tc.check_release_pr(event)
            self.assertEqual(before, manifest.read_bytes())
            self.assertEqual(list(fixture.iterdir()), [manifest])



class LabelSetup(unittest.TestCase):
    def load_labels(self):
        spec = importlib.util.spec_from_file_location('labels', ROOT / 'scripts/test-labels.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def test_definitions_match_resolver_and_documentation(self):
        definitions = self.load_labels().definitions()
        expected = {f'test:{c}' for c in tc.CLASSES} | {f'no-{c}' for c in tc.CLASSES}
        expected |= {'release:fast', 'release:full'}
        self.assertEqual({row[0] for row in definitions}, expected)
        self.assertEqual(len(definitions), 10)
        documentation = (ROOT / 'docs/test-classes.md').read_text()
        for name, color, description in definitions:
            self.assertIn(name, documentation)
            self.assertRegex(color, '^[0-9A-F]{6}$')
            self.assertTrue(description)

    def test_dry_run_and_idempotent_apply_commands_with_fake_gh(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            log = root / 'gh.jsonl'
            gh = root / 'gh'
            gh.write_text('#!' + sys.executable + '\nimport sys,json\n'
                          + f'with open({str(log)!r}, "a") as f: f.write(json.dumps(sys.argv[1:])+"\\n")\n')
            gh.chmod(0o755)
            env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ['PATH'])
            command = [sys.executable, str(ROOT / 'scripts/test-labels.py'), '--repo', 'fixture/repo']
            dry = subprocess.run(command, env=env, capture_output=True, text=True, check=True)
            self.assertEqual(len(dry.stdout.splitlines()), 10)
            self.assertFalse(log.exists())
            for _ in range(2):
                subprocess.run([*command, '--apply'], env=env, check=True)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual(calls[:10], calls[10:])
            self.assertEqual(calls[:10], [row[1:] for row in self.load_labels().commands('fixture/repo')])
            self.assertTrue(all(row[:2] == ['label', 'create'] and '--force' in row for row in calls))

    def test_release_checks_profile_label_before_any_source_or_remote_write(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            scripts = root / 'scripts'
            scripts.mkdir()
            for name in ('release.sh', 'test-labels.py', 'test-classes.py'):
                shutil.copy2(ROOT / 'scripts' / name, scripts / name)
            gh = root / 'gh'
            gh.write_text('#!' + sys.executable + '\nimport sys\n'
                          'print("fixture/repo" if sys.argv[1:3] == ["repo", "view"] else "[]")\n')
            gh.chmod(0o755)
            env = {k: v for k, v in os.environ.items() if not k.startswith(('GH_', 'GITHUB_'))}
            env['PATH'] = str(root) + os.pathsep + env['PATH']
            for profile in ('fast', 'full'):
                result = subprocess.run(['bash', str(scripts / 'release.sh'), '1.2.3', '--compat',
                                         'unchanged', '--profile', profile, '--yes'],
                                        env=env, cwd=root, capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('make test-labels APPLY=1', result.stderr)
                self.assertIn('release:' + profile, result.stderr)
            self.assertEqual(sorted(p.name for p in root.iterdir()), ['gh', 'scripts'])

    def test_release_resolver_has_only_the_required_api_read_permission_and_runner_token(self):
        text = (ROOT / '.github/workflows/release.yml').read_text()
        resolver = text[text.index('  resolve:'):text.index('  # This also provides')]
        self.assertIn('      contents: write # to create the tag', resolver)
        self.assertIn('      pull-requests: read', resolver)
        self.assertIn('          GH_TOKEN: ${{ github.token }}', resolver)
        self.assertIn('          GH_REPO: ${{ github.repository }}', resolver)
        self.assertNotIn('pull-requests: write', text)
        self.assertNotIn('freeze-release-policy', (ROOT / 'scripts/release.sh').read_text())
        self.assertNotIn('release-test-policy:', (ROOT / 'Makefile').read_text())

    def test_label_only_release_pr_events_select_ci_without_a_metadata_commit(self):
        version = tc.re.search(r'^version = "([^"]+)"$', (ROOT / 'Cargo.toml').read_text(), tc.re.M).group(1)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            event = root / 'event.json'
            output = root / 'plan.json'
            for label, selected in [('release:fast', tc.FAST), ('release:full', list(tc.CLASSES)),
                                    ('test:perf', ['perf'])]:
                event.write_text(json.dumps({'pull_request': {'labels': [{'name': label}],
                                 'head': {'ref': 'release/v' + version}, 'base': {'ref': 'main'}}}))
                subprocess.run([sys.executable, str(ROOT / 'scripts/test-classes.py'), 'plan',
                                '--context', 'pr', '--event', str(event), '--output', str(output)],
                               check=True, capture_output=True)
                self.assertEqual(tc.read(output)['selected'], selected)
                self.assertEqual(tc.read(output)['version'], version)
            self.assertFalse((ROOT / '.config/release-tests.json').exists())


class SealedEvidence(unittest.TestCase):
    def test_fresh_base_workflow_entry_records_timing_without_a_target_directory(self):
        workflow = (ROOT / '.github/workflows/validate.yml').read_text()
        base = workflow.split('\n  base:\n', 1)[1].split('\n  classes:\n', 1)[0]
        entry = base.split('      - name: Always-on format, lint, policy and binary smoke\n', 1)[1]
        entry = entry.split('\n      - name:', 1)[0]
        metrics_path = tc.re.search(r'^          AF_CI_METRICS: (.+)$', entry, tc.re.M).group(1)
        recipe = entry.split('        run: |\n', 1)[1]
        recipe = '\n'.join(line[10:] for line in recipe.splitlines())
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / 'source'
            scripts = source / 'scripts'
            scripts.mkdir(parents=True)
            shutil.copyfile(ROOT / 'Makefile', source / 'Makefile')
            for name in ('ci-step.py', 'test-class-python.py'):
                shutil.copyfile(ROOT / 'scripts' / name, scripts / name)
            # Keep the real workflow/Make/timing entrypoints; stand in for the expensive payloads.
            sample = ('import unittest\nclass Case(unittest.TestCase):\n'
                      '    def test_recorded(self): self.assertTrue(True)\nunittest.main()\n')
            for name in ('test-test-classes.py', 'test-release-resolve.py', 'test-af-pin.py'):
                (scripts / name).write_text(sample)
            for name in ('test-classes.py', 'changelog-notes.py', 'af-pin.py'):
                (scripts / name).write_text('pass\n')
            tools = root / 'bin'
            tools.mkdir()
            (tools / 'python3').symlink_to(sys.executable)
            cargo = tools / 'cargo'
            cargo.write_text(f'#!{sys.executable}\nimport json, os, pathlib, sys\n'
                             'if sys.argv[1] == "fmt":\n'
                             '    assert sys.argv[1:] == ["fmt", "--all", "--", "--check"]\n'
                             '    assert not pathlib.Path("target").exists()\n'
                             'with open(os.environ["FAKE_CARGO_CALLS"], "a") as stream:\n'
                             '    stream.write(json.dumps(sys.argv[1:]) + "\\n")\n')
            cargo.chmod(0o755)
            env = {k: v for k, v in os.environ.items()
                   if not k.startswith(('AF_', 'GH_', 'GITHUB_', 'CARGO_', 'RUST'))}
            calls = root / 'cargo-calls.jsonl'
            env.update(PATH=str(tools) + os.pathsep + env['PATH'], AF_CI_METRICS=metrics_path,
                       FAKE_CARGO_CALLS=str(calls))
            self.assertFalse((source / 'target').exists())
            result = subprocess.run(['bash', '-e', '-c', recipe], cwd=source, env=env,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            records = [json.loads(line) for line in (source / metrics_path).read_text().splitlines()]
            self.assertEqual([r['label'] for r in records], ['fmt', 'lint', 'lint-diagnostic-features',
                             'release-resolution', 'changelog-notes', 'af-pin-tests', 'af-pin',
                             'test-classes', 'test-catalog', 'cli-smoke'])
            self.assertTrue(all(r['schema'] == 'af.ci-step/1' and r['exit_code'] == 0
                                and r['elapsed_seconds'] >= 0 and r['started_at'] for r in records))
            self.assertEqual([json.loads(line)[0] for line in calls.read_text().splitlines()],
                             ['fmt', 'clippy', 'clippy', 'run'])

    def test_real_make_recording_paths_write_only_external_gate_outputs(self):
        # A chmod seal is meaningful only as the unprivileged gate user; never skip this proof.
        self.assertNotEqual(os.geteuid(), 0, 'run policy tests as the unprivileged gate user')
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / 'sealed source'
            scripts = source / 'scripts'
            scripts.mkdir(parents=True)
            shutil.copy2(ROOT / 'Makefile', source / 'Makefile')
            for name in ('ci-step.py', 'test-class-python.py'):
                shutil.copy2(ROOT / 'scripts' / name, scripts / name)
            # Exercise all real Make recipes that invoke the recorder with tiny unittest inputs.
            sample = ('import unittest\nclass Case(unittest.TestCase):\n'
                      '    def test_recorded(self): self.assertTrue(True)\nunittest.main()\n')
            for name in ('test-test-classes.py', 'test-release-resolve.py', 'test-af-pin.py'):
                (scripts / name).write_text(sample)
            for name in ('test-classes.py', 'changelog-notes.py', 'af-pin.py'):
                (scripts / name).write_text('pass\n')
            before = {str(p.relative_to(source)): p.read_bytes() for p in source.rglob('*') if p.is_file()}
            files = [p for p in source.rglob('*') if p.is_file()]
            for p in files:
                p.chmod(0o444)
            scripts.chmod(0o555)
            source.chmod(0o555)
            try:
                with self.assertRaises(PermissionError):
                    (source / 'forbidden').write_text('no')
                output = root / 'external gate output'
                output.mkdir()
                env = {k: v for k, v in os.environ.items()
                       if not k.startswith(('AF_', 'GH_', 'GITHUB_'))}
                metrics = output / 'timings/ci-metrics.jsonl'
                env.update(AF_GATE_NEXTEST_RUN=str(output), CARGO_TARGET_DIR=str(root / 'external target'),
                           AF_CI_METRICS=str(metrics))
                result = subprocess.run(['make', 'base-check'], cwd=source, env=env,
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(len(list((output / 'base-evidence').glob('*.xml'))), 3)
                records = [json.loads(line) for line in metrics.read_text().splitlines()]
                self.assertEqual(len(records), 6)
                self.assertTrue(all(r['exit_code'] == 0 for r in records))
                after = {str(p.relative_to(source)): p.read_bytes() for p in source.rglob('*') if p.is_file()}
                self.assertEqual(before, after)
                self.assertFalse((source / 'target').exists())
                self.assertEqual(list(source.rglob('__pycache__')), [])
            finally:
                source.chmod(0o755)
                scripts.chmod(0o755)
                for p in files:
                    p.chmod(0o644)


class Catalog(unittest.TestCase):
    def test_source_and_representative_fixture_partition(self):
        tc.audit_source()
        fixture = tc.read(ROOT / 'fixtures/test-classes/inventory.json')
        entries = tc.partition(fixture)
        self.assertEqual({e['class'] for e in entries}, set(tc.CLASSES))
        self.assertEqual(len(entries), sum(len(s['testcases']) for s in fixture['rust-suites'].values()))
        for c in tc.CLASSES:
            expression = tc.filterset(entries, [c])
            self.assertIn('binary_id(', expression)
        self.assertEqual(tc.classify('af::it', 'perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample_and_refuse_every_tampered_harness'), 'perf')
        self.assertEqual(tc.classify('af::it', 'cli_surface::usage_errors_name_only_the_failing_command'), 'uix')
        self.assertEqual(tc.classify('af::it', 'project::workflow'), 'e2e')
        self.assertEqual(tc.classify('af::bin/af', 'tui::tests::navigation'), 'uix')

    def test_unknown_module_binary_and_overlap_refused(self):
        for b, n in [('new::binary', 'test'), ('af::it', 'new_module::test')]:
            with self.assertRaises(ValueError):
                tc.classify(b, n)
        cat = tc.catalog()
        cat['overrides']['af::bin/af']['tui::tests::'] = 'functional'
        with self.assertRaises(ValueError):
            tc.classify('af::bin/af', 'tui::tests::navigation', cat)

    def test_a_new_empty_binary_requires_catalog_review(self):
        fixture = tc.read(ROOT / 'fixtures/test-classes/inventory.json')
        fixture['rust-suites']['unreviewed::empty'] = {'status': 'listed', 'testcases': {}}
        with self.assertRaisesRegex(ValueError, 'unclassified test binary'):
            tc.partition(fixture)

    def test_unreviewed_ignored_test_and_schema_drift_fail(self):
        fixture = tc.read(ROOT / 'fixtures/test-classes/inventory.json')
        first = next(iter(fixture['rust-suites'].values()))
        next(iter(first['testcases'].values()))['ignored'] = True
        with self.assertRaises(ValueError):
            tc.partition(fixture)
        tc.schema_parity()
        for schema in tc.schema_documents().values():
            self.assertFalse(schema['additionalProperties'])


class Gates(unittest.TestCase):
    def good(self, plan):
        return {'resolve': {'result': 'success', 'outputs': {'plan': json.dumps(plan, separators=(',', ':'))}},
                'base': {'result': 'success'}, 'classes': {'result': 'success'},
                'container-probes': {'result': 'success' if 'e2e' in plan['selected'] else 'skipped'}}

    def test_all_required_results_fail_closed(self):
        plan = tc.resolve(context='pr')
        good = self.good(plan)
        tc.aggregate(plan, good)
        for key in good:
            for state in ('failure', 'cancelled', 'skipped', ''):
                bad = copy.deepcopy(good); bad[key]['result'] = state
                with self.subTest(key=key, state=state), self.assertRaises(ValueError):
                    tc.aggregate(plan, bad)
            bad = copy.deepcopy(good); del bad[key]
            with self.assertRaises(ValueError):
                tc.aggregate(plan, bad)
        bad = copy.deepcopy(good); bad['resolve']['outputs'] = {}
        with self.assertRaises(ValueError):
            tc.aggregate(plan, bad)
        plan = tc.resolve(['test:perf'], 'pr')
        tc.aggregate(plan, self.good(plan))

    def test_selected_platform_receipts_are_exact_and_complete(self):
        plan = tc.resolve(['test:uix'])
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with self.assertRaises(ValueError):
                tc.receipts(plan, root, ['Linux', 'macOS'])
            receipt = {'schema': 'af.test-class-receipt/1', 'plan': plan,
                       'platform': 'Linux', 'class': 'uix', 'status': 'success'}
            tc.write(root / 'linux/receipt.json', receipt)
            tc.receipts(plan, root, ['Linux'])
            for mutation in [{'status': 'cancelled'}, {'class': 'perf'}, {'unexpected': 1},
                             {'plan': tc.resolve(['test:perf'])}]:
                bad = dict(receipt, **mutation)
                tc.write(root / 'linux/receipt.json', bad)
                with self.assertRaises(ValueError):
                    tc.receipts(plan, root, ['Linux'])
            tc.write(root / 'linux/receipt.json', receipt)
            tc.write(root / 'duplicate/receipt.json', receipt)
            with self.assertRaises(ValueError):
                tc.receipts(plan, root, ['Linux'])


class Metrics(unittest.TestCase):
    def test_aggregate_reports_parallel_envelope_and_disjoint_cost(self):
        plan = tc.resolve(['test:functional', 'test:uix'])
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            def metric(path, value):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(json.dumps(value) + '\n')
            for kind, seconds in [('functional', 10), ('uix', 5)]:
                artifact = root / ('validation-class-Linux-' + kind)
                evidence = artifact / 'class-evidence'
                tc.write(evidence / 'receipt.json', {'schema': 'af.test-class-receipt/1', 'plan': plan,
                         'platform': 'Linux', 'class': kind, 'status': 'success'})
                tc.write(evidence / 'report.json', {'plan': plan, 'classes': {kind: {'passed': 1}}})
                metric(artifact / 'class-metrics.jsonl', {'label': 'class-' + kind,
                         'started_at': '2026-10-25T00:00:00+00:00', 'elapsed_seconds': seconds,
                         'exit_code': 0})
            base = root / 'validation-base-Linux-1'
            metric(base / 'base-metrics.jsonl', {'label': 'fmt', 'started_at': '2026-10-25T00:00:01+00:00',
                                                  'elapsed_seconds': 2, 'exit_code': 0})
            value = tc.aggregate_metrics(plan, root, ['Linux'])['platforms']['Linux']
            self.assertEqual(value['observed_runner_command_seconds'], 15)
            self.assertAlmostEqual(value['classes']['functional']['runner_cost_share'], 10 / 15)
            self.assertAlmostEqual(value['classes']['uix']['runner_cost_share'], 5 / 15)
            self.assertAlmostEqual(sum(row['runner_cost_share'] for row in value['classes'].values()
                                       if row['state'] == 'RAN'), 1)
            self.assertEqual(value['observed_command_envelope_seconds'], 10)
            self.assertEqual(value['parallel_class_critical_path_seconds'], 10)
            self.assertEqual(value['base_command_seconds'], 2)
            self.assertIsNone(value['container_command_seconds'])
            self.assertEqual(value['classes']['perf']['state'], 'NOT RUN')
            self.assertEqual(value['last_finishing_command'], 'class-functional')

    def test_container_cost_and_finish_are_separate_from_class_runner_shares(self):
        plan = tc.resolve(['test:e2e'])
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            artifact = root / 'validation-class-Linux-e2e-1'
            evidence = artifact / 'class-evidence'
            tc.write(evidence / 'receipt.json', {'platform': 'Linux', 'class': 'e2e'})
            tc.write(evidence / 'report.json', {'plan': plan, 'classes': {'e2e': {'passed': 1}}})
            outer = {'label': 'class-e2e', 'started_at': '2026-10-25T00:00:00+00:00',
                     'elapsed_seconds': 5, 'exit_code': 0}
            inner = dict(outer, label='inner-step', elapsed_seconds=3)
            (artifact / 'class-metrics.jsonl').write_text('\n'.join(map(json.dumps, [inner, outer])) + '\n')
            for name, seconds, start in [('base', 2, 1), ('container', 20, 4)]:
                path = root / f'validation-{name}-Linux-1' / f'{name}-metrics.jsonl'
                path.parent.mkdir()
                path.write_text(json.dumps({'label': name, 'started_at': f'2026-10-25T00:00:0{start}+00:00',
                                            'elapsed_seconds': seconds, 'exit_code': 0}) + '\n')
            result = tc.aggregate_metrics(plan, root, ['Linux'])
            value = result['platforms']['Linux']
            self.assertEqual(value['observed_runner_command_seconds'], 5)
            self.assertEqual(value['classes']['e2e']['runner_cost_share'], 1)
            self.assertEqual(value['base_command_seconds'], 2)
            self.assertEqual(value['container_command_seconds'], 20)
            self.assertEqual(value['parallel_class_critical_path_seconds'], 5)
            self.assertEqual(value['observed_command_envelope_seconds'], 24)
            self.assertEqual(value['last_finishing_command'], 'container')
            summary = tc.aggregate_summary(result)
            self.assertIn('class runner command cost 5.000s', summary)
            self.assertIn('Base commands: 2; container commands: 20.', summary)
            self.assertIn('Class runner share', summary)

    def test_missing_skipped_and_parallel_are_not_zero_or_wall_savings(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            junit = root / 'junit.xml'
            junit.write_text('''<testsuites time="7"><testsuite name="af::it">
              <testcase classname="af::it" name="cli_surface::ok" time="2"/>
              <testcase classname="af::it" name="project::skip"><skipped/></testcase>
              <testcase classname="af::it" name="project::bad" time="3"><failure/></testcase>
              </testsuite></testsuites>''')
            steps = root / 'steps.jsonl'
            steps.write_text('\n'.join(json.dumps({'schema': 'af.ci-step/1', 'label': 'class-' + c,
                                                    'elapsed_seconds': seconds})
                                        for c, seconds in [('functional', 10), ('e2e', 20), ('uix', 5)]))
            value = tc.report(tc.resolve(context='pr'), [junit], [steps])
            self.assertEqual(value['parallel_class_critical_path_seconds'], 20)
            self.assertEqual(value['class_runner_seconds'], 35)
            self.assertEqual(value['classes']['e2e']['skipped'], 1)
            self.assertEqual(value['classes']['e2e']['failed'], 1)
            self.assertIsNone(value['classes']['perf']['test_seconds'])
            self.assertIsNone(value['classes']['perf']['not_run'])
            self.assertEqual(value['junit_runs'][0]['wall_seconds'], 7)
            self.assertTrue(value['missing_data'])
            self.assertIsNone(tc.report(tc.resolve(), [junit])['class_runner_seconds'])
            with self.assertRaises(ValueError):
                tc.report(tc.resolve(), [junit, junit])

    def test_replay_demonstrates_four_profiles_without_claiming_measured_savings(self):
        path = ROOT / 'fixtures/test-classes/timing.xml'
        result = tc.report(tc.resolve(), [path], replay=True,
                           provenance={'source_run': 'synthetic-contract-fixture', 'source_sha': 'none'})
        scenarios = result['replay_scenarios']
        self.assertEqual(set(scenarios), {'functional+e2e', 'fast', 'full', 'perf-only'})
        self.assertEqual(scenarios['functional+e2e']['parallel_test_wall_lower_bound'], 3)
        self.assertEqual(scenarios['fast']['parallel_test_wall_lower_bound'], 4)
        self.assertEqual(scenarios['full']['parallel_test_wall_lower_bound'], 20)
        self.assertEqual(scenarios['perf-only']['parallel_test_wall_lower_bound'], 20)
        self.assertTrue(result['replay_estimate'])
        self.assertTrue(result['missing_data'])
        self.assertEqual(result['junit_runs'][0]['wall_seconds'], 30)

    def test_python_unittest_wrapper_records_real_cases(self):
        with tempfile.TemporaryDirectory() as tmp:
            junit = Path(tmp) / 'python.xml'
            result = subprocess.run([sys.executable, str(ROOT / 'scripts/test-class-python.py'),
                                     'test-pr-lines.py', str(junit)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            plan = tc.resolve(['test:functional'])
            metrics = tc.report(plan, [junit])
            self.assertGreater(metrics['classes']['functional']['passed'], 0)
            self.assertEqual(metrics['classes']['functional']['failed'], 0)


if __name__ == '__main__':
    unittest.main()
