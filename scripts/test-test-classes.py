#!/usr/bin/env python3
"""Selection, inventory, release binding, aggregation and evidence contracts, offline."""
import copy
import importlib.util
import itertools
import json
import os
import shutil
import stat
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

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
        self.assertEqual(tc.resolve(context='gate')['selected'], tc.FAST)
        self.assertEqual(tc.resolve(context='release_pr')['selected'], tc.FAST)
        self.assertEqual(tc.event_context({'pull_request': {'base': {'ref': 'af-gate/task/base'}}}), 'gate')
        self.assertEqual(tc.event_context({'pull_request': {'base': {'ref': 'main'}}}), 'pr')
        self.assertEqual(tc.event_context({'pull_request': {'head': {'ref': 'release/v1.2.3'}}}), 'release_pr')
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
            for context in ('pr', 'gate', 'release_pr', 'release', 'local'):
                expected = (positive or set(tc.FAST if context in ('pr', 'gate', 'release_pr') else tc.CLASSES)) - negative
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
            for context in ('pr', 'gate', 'release_pr', 'release', 'local'):
                with self.subTest(labels=labels, context=context), self.assertRaises(ValueError):
                    tc.resolve(labels, context)

    def test_gate_pr_cli_defaults_labels_and_measurement_selection(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            event, output, gh = root / 'event.json', root / 'plan.json', root / 'github-output'
            for labels, selected in [([], tc.FAST), (['no-perf'], tc.FAST),
                                     (['no-uix'], ['functional', 'e2e']),
                                     (['test:perf'], ['perf']),
                                     (['release:full'], list(tc.CLASSES))]:
                event.write_text(json.dumps({'pull_request': {
                    'labels': [{'name': label} for label in labels],
                    'head': {'ref': 'af-gate/task/head'}, 'base': {'ref': 'af-gate/task/base'}}}))
                subprocess.run([sys.executable, str(ROOT / 'scripts/test-classes.py'), 'plan',
                                '--context', 'pr', '--event', str(event), '--output', str(output)],
                               check=True, capture_output=True)
                plan = tc.read(output)
                self.assertEqual(plan['selected'], selected)
                self.assertEqual(plan['context'], 'gate')
                if 'perf' not in selected:
                    self.assertEqual(plan['classes']['perf']['state'], 'NOT RUN')
                gh.write_text('')
                subprocess.run([sys.executable, str(ROOT / 'scripts/test-classes.py'),
                                'measurement-scope', '--event', str(event),
                                '--event-name', 'pull_request', '--github-output', str(gh)],
                               check=True, capture_output=True)
                self.assertEqual(gh.read_text(), 'exempt=' + str('perf' not in selected).lower() + '\n')

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
            self.assertEqual(len(gh.read_text().splitlines()), 4)
            self.assertFalse((ROOT / 'SHOULD_NOT_EXIST').exists())


class Release(unittest.TestCase):
    sha = 'a' * 40

    def binding(self, labels=('release:fast',)):
        return {'schema': tc.BINDING, 'sha': self.sha, 'version': '1.2.3',
                'source_pr': {'repository': 'owner/repo', 'number': 335,
                              'merge_commit_sha': self.sha, 'base': 'main',
                              'head': 'release/v1.2.3', 'merged_at': '2026-10-25T11:36:48Z'},
                'plan': tc.resolve(labels, 'release_pr', self.sha, '1.2.3')}

    def test_captured_tag_rerun_is_immutable(self):
        binding = self.binding()
        self.assertEqual(tc.release_plan(json.dumps(binding), self.sha, '1.2.3')['selected'], tc.FAST)
        binding['source_pr'] = None
        with self.assertRaises(ValueError):
            tc.release_plan(json.dumps(binding), self.sha, '1.2.3')

    def test_direct_manual_tags_default_full(self):
        for annotation in ('', 'v1.2.3', 'manual release'):
            self.assertEqual(tc.release_plan(annotation, self.sha, '1.2.3')['selected'], list(tc.CLASSES))

    def test_binding_context_requires_matching_provenance(self):
        binding = self.binding([])
        binding['plan'] = tc.resolve([], 'release', self.sha, '1.2.3')
        with self.assertRaisesRegex(ValueError, 'release plan binding mismatch'):
            tc.release_plan(json.dumps(binding), self.sha, '1.2.3')
        binding['source_pr'] = None
        self.assertEqual(tc.release_plan(json.dumps(binding), self.sha, '1.2.3')['profile'], 'full')
        binding['plan'] = tc.resolve([], 'release_pr', self.sha, '1.2.3')
        with self.assertRaisesRegex(ValueError, 'release plan binding mismatch'):
            tc.release_plan(json.dumps(binding), self.sha, '1.2.3')

    def test_prior_contract_versions_fail_closed(self):
        binding = self.binding([])
        binding['schema'] = 'af.release-tests/1'
        with self.assertRaisesRegex(ValueError, 'invalid release selection annotation'):
            tc.release_plan(json.dumps(binding), self.sha, '1.2.3')
        plan = tc.resolve()
        plan['schema'] = 'af.test-plan/1'
        with self.assertRaisesRegex(ValueError, 'unsupported test plan'):
            tc.validate_plan(plan)

    def test_release_pr_context_and_bound_plan_are_admitted_by_cli(self):
        plan = self.binding([])['plan']
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / 'plan.json'
            command = [sys.executable, str(ROOT / 'scripts/test-classes.py'), 'plan',
                       '--sha', self.sha, '--version', '1.2.3', '--output', str(output)]
            for args in (['--context', 'release_pr'], ['--bound', json.dumps(plan)]):
                subprocess.run([*command, *args], check=True, capture_output=True)
                self.assertEqual(tc.read(output), plan)

    def test_bound_plan_rejects_an_explicit_mismatched_version(self):
        plan = self.binding([])['plan']
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / 'plan.json'
            result = subprocess.run([sys.executable, str(ROOT / 'scripts/test-classes.py'), 'plan',
                                     '--bound', json.dumps(plan), '--sha', self.sha,
                                     '--version', '1.2.4', '--output', str(output)],
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('bound release plan does not name this version', result.stderr)
            self.assertFalse(output.exists())

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
                    self.assertTrue(tc.resolve(labels, 'release_pr')['selected'])
                event['pull_request']['head']['ref'] = 'release/v1.2.4'
                with self.assertRaises(ValueError):
                    tc.check_release_pr(event)
            self.assertEqual(before, manifest.read_bytes())
            self.assertEqual(list(fixture.iterdir()), [manifest])



class ReleaseProof(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location('release_proof', ROOT / 'scripts/release-validation-proof.py')
        self.proof = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.proof)
        self.sha = 'a' * 40

    def test_explicit_profiles_bind_exact_sha_version_and_catalog(self):
        for profile, selected in [('fast', tc.FAST), ('full', list(tc.CLASSES))]:
            plan = self.proof.proof_plan(self.sha, '1.2.3', 'workflow_dispatch', profile)
            self.assertEqual(plan, tc.resolve(['release:' + profile], 'release', self.sha, '1.2.3'))
            self.assertEqual(plan['selected'], selected)
            self.assertEqual(plan['classes']['perf']['state'], 'NOT RUN' if profile == 'fast' else 'SELECTED')
        for event, profile, sha, version in [('push', 'fast', self.sha, '1.2.3'),
                                              ('workflow_dispatch', '', self.sha, '1.2.3'),
                                              ('workflow_dispatch', 'fast', 'main', '1.2.3'),
                                              ('workflow_dispatch', 'full', self.sha, '')]:
            with self.assertRaises(ValueError):
                self.proof.proof_plan(sha, version, event, profile)

    def test_pr_opt_in_is_exact_and_ordinary_labels_cannot_reduce_proof(self):
        for labels in ([], ['test:functional'], ['release:fast', 'release:full']):
            with self.assertRaises(ValueError):
                self.proof.proof_plan(self.sha, '1.2.3', 'pull_request', event={
                    'pull_request': {'labels': [{'name': label} for label in labels]}})
        for profile in ('fast', 'full'):
            event = {'pull_request': {'labels': [{'name': 'release:' + profile},
                                                {'name': 'no-e2e'}, {'name': 'test:functional'}]}}
            self.assertEqual(self.proof.proof_plan(self.sha, '1.2.3', 'pull_request', event=event),
                             tc.resolve(['release:' + profile], 'release', self.sha, '1.2.3'))

    def test_entrypoint_retains_exact_plan_and_emits_it_only_after_simulation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'Cargo.toml').write_text('[workspace.package]\nversion = "1.2.3"\n')
            output = root / 'evidence'
            github_output = root / 'github-output'
            github_output.touch()
            summary = root / 'summary'
            environment = {'GITHUB_SHA': self.sha, 'GITHUB_EVENT_NAME': 'workflow_dispatch',
                           'GITHUB_OUTPUT': str(github_output), 'GITHUB_STEP_SUMMARY': str(summary)}
            arguments = ['proof', '--profile', 'fast', '--output', str(output)]
            # The resolver simulation has its own real Git integration suite. Here exercise
            # the actual CLI handoff and exact checkout binding, without rerunning it nested.
            with mock.patch.dict(os.environ, environment), mock.patch.object(self.proof, 'ROOT', root), \
                    mock.patch.object(sys, 'argv', arguments), \
                    mock.patch.object(self.proof.subprocess, 'check_output', return_value=self.sha + '\n'), \
                    mock.patch.object(self.proof.tc, 'audit_source'), \
                    mock.patch.object(self.proof, 'simulate', return_value={'tests_run': 22}) as simulate:
                self.proof.main()
                plan = tc.read(output / 'plan.json')
                self.assertEqual(plan, tc.resolve(['release:fast'], 'release', self.sha, '1.2.3'))
                line = github_output.read_text().strip()
                self.assertEqual(json.loads(line.split('=', 1)[1]), plan)
                self.assertEqual(tc.read(output / 'local-binding-results.json')['sha'], self.sha)
                self.assertIn('perf: NOT RUN', summary.read_text())
                self.assertIn('No release publication executed or timed', summary.read_text())
                simulate.assert_called_once_with(output)
                github_output.write_text('')
                simulate.side_effect = ValueError('fixture failed')
                with self.assertRaisesRegex(ValueError, 'fixture failed'):
                    self.proof.main()
                self.assertEqual(github_output.read_text(), '')
                simulate.reset_mock()
                with mock.patch.dict(os.environ, {'GITHUB_SHA': 'b' * 40}):
                    with self.assertRaisesRegex(ValueError, 'checkout differs'):
                        self.proof.main()
                simulate.assert_not_called()

    def test_workflow_keeps_read_only_opt_in_and_shared_release_gates(self):
        text = (ROOT / '.github/workflows/release-validation-proof.yml').read_text()
        trigger = text.split('\njobs:\n', 1)[0]
        self.assertIn("paths:\n      - '.github/workflows/release-validation-proof.yml'", trigger)
        self.assertIn('types: [opened, synchronize, reopened, labeled, unlabeled]', trigger)
        self.assertIn('options: [fast, full]', trigger)
        self.assertIn('permissions:\n  contents: read', trigger)
        for forbidden in ('pull_request_target', 'secrets:', 'secrets.', 'write',
                          'release.yml', 'release.sh', 'git push', 'signing'):
            # The comment describes the boundary; inspect executable YAML only.
            self.assertNotIn(forbidden, '\n'.join(line for line in text.splitlines()
                                                  if not line.startswith('#')))
        self.assertIn('persist-credentials: false', text)
        self.assertIn('PROOF_PROFILE: ${{ inputs.profile }}', text)
        self.assertIn('uses: ./.github/workflows/validate.yml', text)
        self.assertIn('release: true', text)
        self.assertIn('test_plan: ${{ needs.proof.outputs.test_plan }}', text)
        self.assertIn('needs: [proof, validate]', text)
        self.assertIn('always() &&', text)
        self.assertIn('test "$PROOF_RESULT" = success', text)
        self.assertIn('test "$VALIDATION_RESULT" = success', text)
        self.assertIn('if-no-files-found: error', text)
        self.assertIn('local-binding-results.json', text)
        # Every external action uses the same full SHA already reviewed in validation.
        import re
        validation = (ROOT / '.github/workflows/validate.yml').read_text()
        actions = re.findall(r'uses: ([^\s]+@[^\s]+)', text)
        self.assertTrue(actions)
        for action in actions:
            self.assertRegex(action, r'@[a-f0-9]{40}$')
            self.assertIn('uses: ' + action, validation)
        self.assertIn('os: ${{ fromJSON(inputs.release &&', validation)
        self.assertIn('["ubuntu-latest", "macos-latest"]', validation)
        for dependency in ('base', 'classes', 'container-probes', 'telemetry-build-smoke',
                           'telemetry-runtime-smoke'):
            self.assertIn(dependency, validation.split('  lint:', 1)[1].split('    steps:', 1)[0])


class LabelSetup(unittest.TestCase):
    def test_make_release_defaults_fast_and_preserves_full_override(self):
        for args, expected in [([], 'fast'), (['PROFILE=full'], 'full')]:
            result = subprocess.run(['make', '-n', 'release', 'VERSION=1.2.3', 'COMPAT=unchanged',
                                     *args], cwd=ROOT, check=True, capture_output=True, text=True)
            self.assertIn('--profile "' + expected + '"', result.stdout)

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
            default = subprocess.run(['bash', str(scripts / 'release.sh'), '1.2.3', '--compat',
                                      'unchanged', '--yes'], env=env, cwd=root,
                                     capture_output=True, text=True)
            self.assertNotEqual(default.returncode, 0)
            self.assertIn('release:fast', default.stderr)
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
            gh = root / 'github-output'
            for labels, selected in [([], tc.FAST), (['release:fast'], tc.FAST),
                                     (['release:full'], list(tc.CLASSES)), (['test:perf'], ['perf'])]:
                event.write_text(json.dumps({'pull_request': {'labels': [{'name': label} for label in labels],
                                 'head': {'ref': 'release/v' + version}, 'base': {'ref': 'main'}}}))
                subprocess.run([sys.executable, str(ROOT / 'scripts/test-classes.py'), 'plan',
                                '--context', 'pr', '--event', str(event), '--output', str(output)],
                               check=True, capture_output=True)
                self.assertEqual(tc.read(output)['selected'], selected)
                self.assertEqual(tc.read(output)['context'], 'release_pr')
                self.assertEqual(tc.read(output)['version'], version)
                gh.write_text('')
                subprocess.run([sys.executable, str(ROOT / 'scripts/test-classes.py'),
                                'measurement-scope', '--event', str(event),
                                '--event-name', 'pull_request', '--github-output', str(gh)],
                               check=True, capture_output=True)
                self.assertEqual(gh.read_text(), 'exempt=' + str('perf' not in selected).lower() + '\n')
            self.assertFalse((ROOT / '.config/release-tests.json').exists())


class SealedEvidence(unittest.TestCase):
    def test_verify_wrapper_keeps_ownership_and_forwards_explicit_selection(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            scripts = root / 'scripts'
            scripts.mkdir()
            for name in ('verify.sh', 'verify-body.sh', 'gate-owner.py'):
                shutil.copy2(ROOT / 'scripts' / name, scripts / name)
            shutil.copy2(ROOT / 'scripts/test-classes.py', scripts / 'test-classes.py')
            for source in (ROOT / 'scripts').glob('test-*.py'):
                if source.name != 'test-classes.py':
                    shutil.copy2(source, scripts / source.name)
            (root / '.config').mkdir()
            for name in ('test-classes.json', 'test-quarantine.json'):
                shutil.copy2(ROOT / '.config' / name, root / '.config' / name)
            (root / 'schemas').mkdir()
            for name in tc.schema_documents():
                shutil.copy2(ROOT / 'schemas' / name, root / 'schemas' / name)
            for source in (ROOT / 'crates').glob('*/tests/it/main.rs'):
                destination = root / source.relative_to(ROOT)
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, destination)
            workflow = root / '.github/workflows/validate.yml'
            workflow.parent.mkdir(parents=True)
            shutil.copy2(ROOT / '.github/workflows/validate.yml', workflow)
            tools = root / 'bin'
            tools.mkdir()
            make = tools / 'make'
            make.write_text(f'#!{sys.executable}\nimport json, os, pathlib, sys\n'
                            'pathlib.Path(os.environ["CALLS"]).write_text(json.dumps({\n'
                            '"args": sys.argv[1:], "run": os.environ["AF_GATE_NEXTEST_RUN"],\n'
                            '"target": os.environ["AF_GATE_NEXTEST_TARGET"],\n'
                            '"metrics": os.environ["AF_CI_METRICS"]}))\n'
                            'sys.exit(int(os.environ["STATUS"]))\n')
            make.chmod(0o755)
            plan = root / 'selected plan.json'
            tc.write(plan, tc.resolve(['test:uix']))
            calls = root / 'calls.json'
            env = {k: v for k, v in os.environ.items()
                   if not k.startswith(('AF_', 'CARGO_', 'GH_', 'GITHUB_'))}
            target = root / 'owned output'
            env.update(PATH=str(tools) + os.pathsep + env['PATH'],
                       AF_GATE_TARGET_DIR=str(target), CALLS=str(calls), STATUS='0')
            for args, expected in [([], ['check']),
                                   (['--profile', 'full'], ['check']),
                                   (['--test-plan', str(plan)], ['check-classes', f'TEST_PLAN={plan}'])]:
                for status in (0, 23):
                    env['STATUS'] = str(status)
                    result = subprocess.run(['bash', str(scripts / 'verify.sh'), *args], cwd=root,
                                            env=env, capture_output=True, text=True, timeout=10)
                    self.assertEqual(result.returncode, status, result.stdout + result.stderr)
                    observed = tc.read(calls)
                    self.assertEqual(observed['args'], expected)
                    self.assertEqual(observed['target'], str(target))
                    self.assertEqual(Path(observed['run']).parent, target / 'nextest-reports')
                    self.assertEqual(observed['metrics'], observed['run'] + '/ci-metrics.jsonl')
            env['STATUS'] = '0'
            result = subprocess.run(['bash', str(scripts / 'verify.sh'), '--profile', 'fast'],
                                    cwd=root, env=env, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            observed = tc.read(calls)
            fast_plan = Path(observed['run']) / 'test-plan.json'
            self.assertEqual(observed['args'], ['test-build', 'check-classes',
                                                f'TEST_PLAN={fast_plan}'])
            self.assertEqual(observed['metrics'], observed['run'] + '/ci-metrics.jsonl')
            self.assertEqual({key: tc.read(fast_plan)[key]
                              for key in ('profile', 'context', 'selected')},
                             {'profile': 'fast', 'context': 'gate',
                              'selected': ['functional', 'e2e', 'uix']})
            calls.unlink()
            for args in (['--unknown'], ['--profile'], ['--profile', 'perf'],
                         ['--test-plan'], ['--test-plan', str(root / 'absent')]):
                result = subprocess.run(['bash', str(scripts / 'verify.sh'), *args], cwd=root,
                                        env=env, capture_output=True, timeout=10)
                self.assertEqual(result.returncode, 2)
                self.assertIn(b'usage: scripts/verify.sh', result.stderr)
                self.assertFalse(calls.exists())
            self.assertFalse((root / 'target').exists())

    def test_full_gate_build_precedes_python_tests_with_real_step_receipts(self):
        # Keep the actual wrapper, Make graph and receipt writer. Only expensive payloads
        # are stand-ins; never run a nested real gate from this preflight regression.
        preflight = ['task-preflight', 'prereqs', 'gate-owner', 'nextest-gate', 'stress-proof',
                     'test-time-report', 'provider-auth-host', 'pr-report', 'pr-lines',
                     'test-classes', 'test-catalog', 'performance-schemas',
                     'performance-kernel-harness']
        lint = ['fmt', 'lint', 'lint-diagnostic-features']
        tests = ['test', 'performance-lab', 'test-perf-trace', 'test-perf-calibration']
        release = ['release-resolution', 'changelog-notes-tests', 'changelog-notes', 'af-pin-tests',
                   'af-pin']
        expected = [(label, 'preflight') for label in preflight]
        expected += [(label, 'lint') for label in lint] + [('test-build', 'build')]
        expected += [(label, 'tests') for label in tests]
        expected += [(label, 'release') for label in release]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            scripts = root / 'scripts'
            scripts.mkdir()
            shutil.copy2(ROOT / 'Makefile', root / 'Makefile')
            for name in ('verify.sh', 'verify-body.sh', 'gate-owner.py', 'ci-step.py'):
                shutil.copy2(ROOT / 'scripts' / name, scripts / name)
            # All command paths required by the full Make entry must exist in the fixture.
            payloads = ['test-task-preflight.py', 'test-prereqs.py', 'test-gate-owner.py',
                        'test-nextest-gate.py', 'test-stress-proof.py',
                        'test-test-time-report.py',
                        'test-provider-auth-host.py', 'test-check-pr-report.py',
                        'test-pr-lines.py', 'test-class-python.py', 'test-classes.py',
                        'test-performance.py', 'performance/schemas.py', 'performance/cli.py',
                        'nextest-gate.py', 'changelog-notes.py', 'af-pin.py',
                        'test-changelog-notes.py']
            for name in payloads:
                path = scripts / name
                path.parent.mkdir(exist_ok=True)
                path.write_text('pass\n')
            (scripts / 'test-classes.py').write_text(
                'import os, pathlib, subprocess, sys\n'
                'if sys.argv[1] == "plan":\n'
                '    pathlib.Path(sys.argv[sys.argv.index("--output") + 1]).write_text("{}")\n'
                'elif sys.argv[1] == "execute":\n'
                '    for kind in ("functional", "e2e", "uix"):\n'
                '        subprocess.run([sys.executable, "scripts/ci-step.py", "--stage",\n'
                '                        "tests", "class-" + kind, sys.executable, "-c", "pass"],\n'
                '                       check=True)\n')
            tools = root / 'bin'
            tools.mkdir()
            (tools / 'python3').symlink_to(sys.executable)
            cargo = tools / 'cargo'
            cargo.write_text(f'#!{sys.executable}\nimport os, sys\n'
                             'sys.exit(int(os.environ["BUILD_STATUS"]) if '
                             'sys.argv[1:] == ["test", "--locked", "--no-run"] else 0)\n')
            cargo.chmod(0o755)
            env = {k: v for k, v in os.environ.items()
                   if not k.startswith(('AF_', 'GH_', 'GITHUB_', 'CARGO_', 'RUST', 'MAKE'))}
            report = root / 'check.jsonl'
            metrics = root / 'metrics.jsonl'
            env.update(PATH=str(tools) + os.pathsep + env['PATH'],
                       AF_GATE_TARGET_DIR=str(root / 'output'), AF_CHECK_REPORT=str(report),
                       AF_CI_METRICS=str(metrics))
            for runner in ('nextest', 'cargo'):
                for entry, status in (('check', 0), ('check', 19), ('test', 0),
                                      ('fast', 0), ('fast', 19)):
                    with self.subTest(runner=runner, entry=entry, build_status=status):
                        report.unlink(missing_ok=True)
                        metrics.unlink(missing_ok=True)
                        env.update(TEST_RUNNER=runner, BUILD_STATUS=str(status))
                        command = (['bash', str(scripts / 'verify.sh'),
                                    *(['--profile', 'fast'] if entry == 'fast' else [])]
                                   if entry != 'test' else ['make', 'test'])
                        result = subprocess.run(command, cwd=root, env=env,
                                                capture_output=True, text=True, timeout=20)
                        self.assertEqual(result.returncode == 0, status == 0,
                                         result.stdout + result.stderr)
                        records = [json.loads(line) for line in report.read_text().splitlines()]
                        if entry == 'fast':
                            wanted = [('test-build', 'build')]
                            if not status:
                                wanted += [(label, 'release') for label in release]
                                wanted += [(label, 'preflight') for label in
                                           ('gate-owner', 'test-classes', 'test-catalog')]
                                wanted += [(label, 'lint') for label in lint]
                                wanted += [(f'class-{kind}', 'tests') for kind in
                                           ('functional', 'e2e', 'uix')]
                        else:
                            wanted = (expected[:len(preflight) + len(lint) + 1] if status
                                      else expected if entry == 'check'
                                      else [('test-build', 'build'), ('test', 'tests')])
                        self.assertEqual([(r['label'], r['stage']) for r in records], wanted)
                        build = next(r for r in records if r['label'] == 'test-build')
                        self.assertEqual(build['command'], ['cargo', 'test', '--locked', '--no-run'])
                        self.assertEqual(build['exit_code'], status)
                        timed = [json.loads(line) for line in metrics.read_text().splitlines()]
                        self.assertEqual([r['label'] for r in timed],
                                         [r['label'] for r in records])
                        if status:
                            self.assertFalse(any(r['stage'] == 'tests' for r in records))
                            self.assertFalse(any(r['label'].startswith('class-') for r in records))
                            self.assertFalse(any(r['label'].startswith('class-') for r in timed))
                            continue
                        self.assertTrue(all(r['exit_code'] == 0 for r in records))
                        if entry == 'fast':
                            self.assertEqual(timed[0]['command'],
                                             ['cargo', 'test', '--locked', '--no-run'])
                            self.assertEqual(records[0]['stage'], 'build')
                            self.assertLess(0, next(i for i, r in enumerate(records)
                                                    if r['stage'] == 'tests'))
                            continue
                        rust = next(r for r in records if r['label'] == 'test')
                        # nextest starts through the quarantine exclusion (ADR-0264).
                        wanted = (['python3', 'scripts/test-classes.py', 'nextest-gate', '--']
                                  if runner == 'nextest' else ['cargo', 'test'])
                        self.assertEqual(rust['command'][:len(wanted)], wanted)
                        if entry == 'check':
                            python = next(r for r in records if r['label'] == 'performance-lab')
                            self.assertEqual(python['command'], ['python3', 'scripts/test-performance.py'])
                            self.assertLess(records.index(build), records.index(python))

    def test_selected_build_receipts_cover_all_variants_before_any_suite(self):
        # Real native wrapper, Make graph, class runner and receipt writer; only Cargo
        # and suite payloads are stand-ins. This also exercises hosted direct-class entry.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            shutil.copy2(ROOT / 'Makefile', root / 'Makefile')
            shutil.copytree(ROOT / 'scripts', root / 'scripts')
            for directory in ('.config', 'schemas', '.github'):
                shutil.copytree(ROOT / directory, root / directory)
            # AF snapshots are read-only; this disposable fixture rewrites copied files.
            for path in root.rglob('*'):
                if path.is_file() and not path.is_symlink():
                    path.chmod(stat.S_IMODE(path.stat().st_mode) | stat.S_IWUSR)
            scripts = root / 'scripts'
            for name in ('test-class-python.py', 'changelog-notes.py', 'af-pin.py',
                         'performance/schemas.py', 'performance/cli.py'):
                (scripts / name).write_text('pass\n')
            tools = root / 'bin'
            tools.mkdir()
            (tools / 'python3').symlink_to(sys.executable)
            inventory = {'rust-suites': {
                'review-core': {'testcases': {'functional': {'ignored': False},
                                              'perf_trace::perf_case': {'ignored': False}}},
                'review-process': {'testcases': {'e2e_case': {'ignored': False}}},
                'af::it': {'testcases': {'cli_surface::uix_case': {'ignored': False}}},
                'review-pipeline::perf_probe': {'testcases': {
                    'calibration_build_case': {'ignored': False}}}}}
            tc.write(root / 'inventory.json', inventory)
            cargo = tools / 'cargo'
            cargo.write_text(f'#!{sys.executable}\n' + r"""
import json, os, pathlib, sys
if sys.argv[1:3] == ['nextest', 'list']:
    args = sys.argv[1:]
    variant = ('trace' if 'review-core/perf-trace,review-core/perf-calibration' in args
               else 'calibration' if '--features' in args else 'default')
    report = pathlib.Path(os.environ['AF_CHECK_REPORT'])
    records = [json.loads(line) for line in report.read_text().splitlines()]
    with open(os.environ['BUILDS'], 'a') as stream:
        stream.write(json.dumps({'variant': variant, 'records': records}) + '\n')
    print(pathlib.Path('inventory.json').read_text())
    print('compiler diagnostic', file=sys.stderr)
    if os.environ['FAIL_VARIANT'] == variant:
        sys.exit(19)
""")
            cargo.chmod(0o755)
            (scripts / 'nextest-gate.py').write_text("""
import os, pathlib, sys, xml.etree.ElementTree as ET
args = sys.argv[1:]
expression = args[args.index('-E') + 1]
if '--features' in args:
    cases = ([('review-core', 'perf_trace::perf_case')] if 'test(perf_)' in expression
             else [('review-pipeline::perf_probe', 'calibration_build_case')])
elif 'review-process' in expression:
    cases = [('review-process', 'e2e_case')]
elif 'af::it' in expression:
    cases = [('af::it', 'cli_surface::uix_case')]
elif 'functional' in expression:
    cases = [('review-core', 'functional')]
else:
    cases = [('review-core', 'perf_trace::perf_case'),
             ('review-pipeline::perf_probe', 'calibration_build_case')]
root = ET.Element('testsuites')
suite = ET.SubElement(root, 'testsuite')
for binary, name in cases:
    ET.SubElement(suite, 'testcase', classname=binary, name=name, time='0.01')
path = (pathlib.Path(os.environ['AF_GATE_NEXTEST_RUN']) / 'store/ci/junit.xml'
        if os.getenv('AF_GATE_NEXTEST_RUN') else pathlib.Path('target/nextest/ci/junit.xml'))
path.parent.mkdir(parents=True, exist_ok=True)
ET.ElementTree(root).write(path, encoding='unicode')
""")
            env = {k: v for k, v in os.environ.items()
                   if not k.startswith(('AF_', 'GH_', 'GITHUB_', 'CARGO_', 'RUST', 'MAKE'))}
            report, builds = root / 'check.jsonl', root / 'builds.jsonl'
            env.update(PATH=str(tools) + os.pathsep + env['PATH'],
                       AF_GATE_TARGET_DIR=str(root / 'output'), AF_CHECK_REPORT=str(report),
                       BUILDS=str(builds))
            full = list(tc.CLASSES)
            scenarios = [('native', selected, selected[0], failed)
                         for selected in (['functional'], ['perf'], full)
                         for failed in (('none', 'default', 'trace', 'calibration')
                                        if 'perf' in selected else ('none', 'default'))]
            # Hosted jobs retain the full plan in receipts but compile only their class.
            # A broken perf variant cannot block any hosted non-perf job.
            scenarios += [('hosted', full, kind, failed) for kind in tc.CLASSES
                          for failed in ('none', 'default', 'trace', 'calibration')]
            for entry, selected, kind, failed in scenarios:
                with self.subTest(entry=entry, selected=selected, kind=kind, failed=failed):
                    report.unlink(missing_ok=True)
                    builds.unlink(missing_ok=True)
                    shutil.rmtree(root / 'output', ignore_errors=True)
                    out = root / 'output/hosted'
                    plan = tc.resolve(['test:' + c for c in selected])
                    tc.write(root / 'plan.json', plan)
                    env['FAIL_VARIANT'] = failed
                    command = (['bash', 'scripts/verify.sh', '--test-plan', str(root / 'plan.json')]
                               if entry == 'native' else [sys.executable, 'scripts/test-classes.py',
                               'run', '--plan', str(root / 'plan.json'), '--class', kind,
                               '--output', str(out)])
                    result = subprocess.run(command, cwd=root, env=env, capture_output=True,
                                            text=True, timeout=60)
                    includes_perf = 'perf' in selected if entry == 'native' else kind == 'perf'
                    required = ['default', 'trace', 'calibration'] if includes_perf else ['default']
                    failed_required = failed in required
                    self.assertEqual(result.returncode == 0, not failed_required,
                                     result.stdout + result.stderr)
                    records = [json.loads(line) for line in report.read_text().splitlines()]
                    compiled = [json.loads(line) for line in builds.read_text().splitlines()]
                    # At each compilation in the first class there is still no build
                    # receipt, including when compiling its last feature variant.
                    variants = required
                    if failed_required:
                        variants = variants[:variants.index(failed) + 1]
                    self.assertEqual([r['variant'] for r in compiled[:len(variants)]], variants)
                    for r in compiled[:len(variants)]:
                        self.assertFalse(any(s['stage'] == 'build' for s in r['records']))
                    build = next(r for r in records if r['label'] == 'class-build')
                    self.assertEqual(build['stage'], 'build')
                    if failed_required:
                        self.assertNotEqual(build['exit_code'], 0)
                        # execute's outer class interval records its failure too;
                        # it must not hide a suite start or claim class success.
                        test_steps = [r for r in records if r['stage'] == 'tests']
                        self.assertTrue(all(r['label'] == 'class-' + selected[0]
                                            and r['exit_code'] != 0 for r in test_steps))
                        self.assertFalse(list((root / 'output').rglob('receipt.json')))
                        self.assertFalse(any(r['stage'] == 'build' and r['exit_code'] == 0
                                             for r in records))
                        continue
                    if entry == 'hosted':
                        self.assertEqual([r['variant'] for r in compiled], required)
                    self.assertEqual(build['exit_code'], 0)
                    suites = [r for r in records if r['stage'] == 'tests']
                    self.assertTrue(suites)
                    self.assertTrue(all(records.index(r) > records.index(build) for r in suites))
                    self.assertIn('compiler diagnostic', result.stderr)
                    inventories = list((root / 'output').rglob('nextest-list.json'))
                    self.assertTrue(inventories)
                    self.assertTrue(all(tc.read(p) == inventory for p in inventories))
                    receipts = list((root / 'output').rglob('receipt.json'))
                    self.assertEqual(len(receipts), len(selected) if entry == 'native' else 1)
                    for receipt in receipts:
                        self.assertEqual(tc.read(receipt)['plan'], plan)
                    for p in (root / 'output').rglob('feature-*-list.json'):
                        self.assertEqual(tc.read(p), inventory)

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
            for name in ('test-test-classes.py', 'test-release-resolve.py', 'test-af-pin.py', 'test-gate-owner.py',
                         'test-changelog-notes.py'):
                (scripts / name).write_text(sample)
            for name in ('test-classes.py', 'changelog-notes.py', 'af-pin.py'):
                (scripts / name).write_text('pass\n')
            (scripts / 'release-smoke.sh').write_text('[ "$1" = target/debug/af ]\n')
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
                             'release-resolution', 'changelog-notes-tests', 'changelog-notes',
                             'af-pin-tests', 'af-pin',
                             'gate-owner', 'test-classes', 'test-catalog', 'cli-smoke', 'release-smoke'])
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
            for name in ('test-test-classes.py', 'test-release-resolve.py', 'test-af-pin.py', 'test-gate-owner.py',
                         'test-changelog-notes.py'):
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
                self.assertEqual(len(list((output / 'base-evidence').glob('*.xml'))), 5)
                records = [json.loads(line) for line in metrics.read_text().splitlines()]
                self.assertEqual(len(records), 8)
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
    def test_build_scope_does_not_relax_plan_or_class_admission(self):
        from unittest import mock

        plan = tc.resolve(['test:functional'])
        tampered = copy.deepcopy(plan)
        tampered['selected'].append('perf')
        with mock.patch.object(tc.subprocess, 'run') as cargo:
            for value, kind, scope in ((plan, 'perf', 'class'), (plan, 'perf', 'plan'),
                                       (tampered, 'functional', 'class'),
                                       (tampered, 'functional', 'plan'),
                                       (plan, 'functional', 'unknown')):
                with self.subTest(kind=kind, scope=scope), self.assertRaises(ValueError):
                    tc.build_inventory(value, kind, Path('unused'), scope)
            cargo.assert_not_called()

    def test_gate_owner_is_always_on_and_precedes_selected_cargo_inventory(self):
        cat = tc.catalog()
        self.assertEqual(cat['scripts']['test-gate-owner.py'], 'functional')
        self.assertIn('test-gate-owner.py', cat['always_on_scripts'])
        self.assertEqual(cat['hosted_checks'], {'container-probes': 'e2e', 'telemetry-build-smoke': 'perf',
                                                'telemetry-runtime-smoke': 'perf'})
        source = (ROOT / 'scripts/test-classes.py').read_text()
        runner = source[source.index('def run_class('):source.index('def main(')]
        self.assertLess(runner.index("'test-gate-owner.py'"), runner.index("'build-inventory'"))
        recipes = (ROOT / 'Makefile').read_text()
        self.assertIn('gate-owner python3 scripts/test-gate-owner.py', recipes.split('preflight-check:', 1)[1])
        self.assertIn('test-gate-owner.py', recipes.split('base-check: release-check', 1)[1])
        self.assertEqual((ROOT / 'scripts/verify.sh').read_text().splitlines()[-1],
                         'exec python3 "$scripts_dir/gate-owner.py" bash "$scripts_dir/verify-body.sh" "$@"')

    def test_unclassified_hosted_validation_job_refuses_audit(self):
        from unittest import mock

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            workflow = root / '.github/workflows/validate.yml'
            workflow.parent.mkdir(parents=True)
            source = (ROOT / '.github/workflows/validate.yml').read_text()
            cat = tc.catalog()
            cat.update(scripts={}, utilities=[], always_on_scripts=[])
            with mock.patch.object(tc, 'ROOT', root), mock.patch.object(tc, 'schema_parity'):
                workflow.write_text(source)
                tc.audit_source(cat)
                workflow.write_text(source + '\n  unclassified-smoke:\n    runs-on: ubuntu-latest\n')
                with self.assertRaisesRegex(ValueError, 'hosted validation job catalog drift'):
                    tc.audit_source(cat)

    def test_source_and_representative_fixture_partition(self):
        tc.audit_source()
        fixture = tc.read(ROOT / 'fixtures/test-classes/inventory.json')
        entries = tc.partition(fixture)
        self.assertEqual({e['class'] for e in entries}, set(tc.CLASSES))
        self.assertEqual(len(entries), sum(len(s['testcases']) for s in fixture['rust-suites'].values()))
        for c in tc.CLASSES:
            expression = tc.filterset(entries, [c])
            self.assertIn('binary_id(', expression)
        self.assertEqual(tc.classify('af::it', 'perf_kernel_measure::guarded_kernel_experiments_refuse_every_tampered_harness'), 'perf')
        self.assertEqual(tc.classify('af::it', 'perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample'), 'perf')
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


class Quarantine(unittest.TestCase):
    # Every guard reads this date, never the clock.
    today = '2026-10-29'
    held = 'af::it cli_surface::usage_errors_name_only_the_failing_command'

    def entry(self, **fields):
        value = {'state': 'quarantined', 'since': '2026-10-20', 'expires': '2026-11-03',
                 'evidence': ['gate-flake-1', 'gate-flake-2'], 'issue': 466}
        value.update(fields)
        return {k: v for k, v in value.items() if v is not None}

    def held_file(self, tests):
        """A test-local quarantine file in place of the shipped one, and an explicit today."""
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        path = Path(tmp.name) / 'test-quarantine.json'
        tc.write(path, {'schema': 'af.test-quarantine/1', 'tests': tests})
        for patch in (mock.patch.object(tc, 'QUARANTINE', path),
                      mock.patch.dict(os.environ, {'AF_TEST_QUARANTINE_TODAY': self.today})):
            patch.start()
            self.addCleanup(patch.stop)

    def inventory(self):
        fixture = tc.read(ROOT / 'fixtures/test-classes/inventory.json')
        suites = fixture['rust-suites']
        for name in ('cli_surface::help_lists_every_command', 'cli_surface::version_prints_one_line'):
            suites['af::it']['testcases'][name] = {'ignored': False}
        suites['review-process::cancellation']['testcases']['cancellation_after_exit_is_a_noop'] = {
            'ignored': False}
        return fixture

    @staticmethod
    def selects(expression, binary, name):
        for clause in expression.split(' | '):
            prefix = f'(binary_id(={binary}) & test(/'
            if clause.startswith(prefix):
                return tc.re.match(clause[len(prefix):-len('/))')], name) is not None
        return False

    def test_the_shipped_file_is_empty_and_passes_every_guard(self):
        self.assertEqual(tc.read(tc.QUARANTINE), {'schema': 'af.test-quarantine/1', 'tests': {}})
        self.assertEqual(tc.quarantine(today=tc.dt.date(2026, 10, 29)), {})
        self.assertIn('test-quarantine-v1.json', tc.schema_documents())
        with mock.patch.dict(os.environ, {'AF_TEST_QUARANTINE_TODAY': self.today}):
            tc.audit_source()
            # An empty quarantine selects exactly what the catalog alone selects.
            fixture = self.inventory()
            self.assertEqual(tc.partition(fixture), tc.partition(fixture, held={}))
            self.assertTrue(all(e['class'] in tc.CLASSES for e in tc.partition(fixture)))

    def test_a_quarantined_test_leaves_the_fast_and_full_filters_but_its_siblings_stay(self):
        other = 'review-process::cancellation cancellation_before_spawn_cannot_execute_the_command'
        self.held_file({self.held: self.entry(), other: self.entry(state='flaky', issue=None)})
        entries = tc.partition(self.inventory())
        classes = {e['binary'] + ' ' + e['test']: e['class'] for e in entries}
        self.assertEqual(classes[self.held], 'quarantine')
        self.assertEqual(classes[other], 'quarantine')
        self.assertNotIn('quarantine', tc.CLASSES)
        for plan in (tc.resolve(['release:fast'], 'gate'), tc.resolve(['release:full'], 'pr')):
            self.assertNotIn('quarantine', plan['selected'])
            expression = tc.filterset(entries, plan['selected'])
            with self.subTest(profile=plan['profile']):
                self.assertFalse(self.selects(expression, 'af::it', self.held.split(' ')[1]))
                self.assertFalse(self.selects(expression, *other.split(' ')))
                for binary, name in [('af::it', 'cli_surface::help_lists_every_command'),
                                     ('af::it', 'cli_surface::version_prints_one_line'),
                                     ('review-process::cancellation', 'cancellation_after_exit_is_a_noop'),
                                     ('review-core::schema_parity',
                                      'a_gate_cleanup_is_one_closed_transition_referencing_no_artifact')]:
                    self.assertTrue(self.selects(expression, binary, name), (binary, name))
        full = tc.filterset(entries, list(tc.CLASSES))
        self.assertTrue(self.selects(full, 'af::it',
                                     'perf_kernel_measure::guarded_kernel_experiments_refuse_every_tampered_harness'))
        # The class report counts no quarantined case in any class row or timing.
        with tempfile.TemporaryDirectory() as tmp:
            junit = Path(tmp) / 'junit.xml'
            junit.write_text('<testsuites><testsuite name="af::it">'
                             '<testcase classname="af::it" name="cli_surface::usage_errors_name_only_the_failing_command"'
                             ' time="9"><failure/></testcase>'
                             '<testcase classname="af::it" name="cli_surface::version_prints_one_line" time="1"/>'
                             '</testsuite></testsuites>')
            value = tc.report(tc.resolve(['release:full'], 'pr'), [junit], inventory=entries)
        self.assertEqual(value['classes']['uix']['count'], 1)
        self.assertEqual(value['classes']['uix']['failed'], 0)
        self.assertEqual(value['classes']['uix']['test_seconds'], 1)
        self.assertEqual(sum(row['inventory_count'] for row in value['classes'].values()), len(entries) - 2)

    def refusal(self, tests, *needles, inventory=None):
        self.held_file(tests)
        with self.assertRaises(ValueError) as caught:
            if inventory is None:
                with mock.patch.object(sys, 'argv', ['test-classes.py', 'audit']):
                    tc.main()  # The `test-catalog` step of make check.
            else:
                tc.partition(inventory)
        for needle in (*needles, 'fix: '):
            self.assertIn(needle, str(caught.exception))

    def test_each_guard_refuses_naming_the_entry_and_the_fix(self):
        cases = [
            ({self.held: self.entry(owner='me')}, "unknown field ['owner']"),
            ({self.held: self.entry(state='skipped')}, "state 'skipped'"),
            ({self.held: self.entry(state=None)}, "missing field ['state']"),
            ({self.held: self.entry(since='2026-10-32')}, '`since`'),
            ({self.held: self.entry(expires='03.11.2026')}, '`expires`'),
            ({self.held: self.entry(expires='2026-10-20')}, 'is not after since'),
            ({self.held: self.entry(expires='2026-11-04')}, 'at most 14 days'),
            ({self.held: self.entry(evidence=[])}, 'evidence'),
            ({self.held: self.entry(evidence=['no spaces'])}, 'evidence'),
            ({self.held: self.entry(issue=0)}, 'issue 0'),
            ({self.held: self.entry(issue=True)}, 'issue True'),
            ({self.held: self.entry(since='2026-10-14', expires='2026-10-28')},
             'expired on 2026-10-28 (today is 2026-10-29)'),
            ({'af::it perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample': self.entry()},
             'an #[ignore] test cannot be quarantined'),
            ({'af::it no_such_module::a_test': self.entry()}, 'unclassified module'),
            ({'af::it': self.entry()}, 'nextest identity'),
        ]
        for tests, needle in cases:
            with self.subTest(needle=needle):
                self.refusal(tests, '.config/test-quarantine.json: ' + next(iter(tests)), needle)
        # Today is the last valid day; a date after since is required, not the clock.
        self.held_file({self.held: self.entry(since='2026-10-15', expires='2026-10-29')})
        self.assertEqual(list(tc.quarantine()), [self.held])
        # The window is checked as a difference, so the last representable dates still pass
        # or refuse with the entry's message.
        self.held_file({self.held: self.entry(since='9999-12-20', expires='9999-12-21')})
        self.assertEqual(list(tc.quarantine()), [self.held])
        self.refusal({self.held: self.entry(since='9999-12-01', expires='9999-12-31')},
                     '.config/test-quarantine.json: ' + self.held, 'at most 14 days')

    def test_the_file_shape_and_the_entry_bound_are_guarded(self):
        for value in ({'schema': 'af.test-quarantine/2', 'tests': {}}, {'schema': 'af.test-quarantine/1'},
                      {'schema': 'af.test-quarantine/1', 'tests': {}, 'extra': 1}):
            with self.subTest(value=value), self.assertRaisesRegex(ValueError, 'unsupported test quarantine.*fix:'):
                tc.quarantine(today=tc.dt.date(2026, 10, 29), value=value)
        many = {f'af::it cli_surface::case_{i}': self.entry() for i in range(21)}
        self.refusal(many, '21 entries, at most 20', 'cli_surface::case_0')
        twenty = dict(list(many.items())[:20])
        self.held_file(twenty)
        self.assertEqual(len(tc.quarantine()), 20)
        with mock.patch.dict(os.environ, {'AF_TEST_QUARANTINE_TODAY': '29.10.2026'}), \
                self.assertRaisesRegex(ValueError, 'AF_TEST_QUARANTINE_TODAY'):
            tc.quarantine()

    def test_inventory_guards_refuse_absent_and_ignored_identities(self):
        absent = 'af::it cli_surface::renamed_away'
        self.refusal({absent: self.entry()}, '.config/test-quarantine.json: ' + absent,
                     'not in the test inventory', inventory=self.inventory())
        fixture = self.inventory()
        fixture['rust-suites']['af::it']['testcases'][self.held.split(' ')[1]]['ignored'] = True
        self.refusal({self.held: self.entry()}, '.config/test-quarantine.json: ' + self.held,
                     'an #[ignore] test cannot be quarantined', inventory=fixture)

    def test_a_feature_only_identity_is_refused_as_outside_the_default_feature_inventory(self):
        feature_only = ('review-pipeline::perf_probe calibration_build::'
                        'calibration_build_probe_injects_once_inside_the_region_and_keeps_its_oracle')
        needles = ('covers the default-feature inventory only',
                   'fix the test, or exclude it from its feature selector by hand')
        self.refusal({feature_only: self.entry()}, '.config/test-quarantine.json: ' + feature_only,
                     'calibration feature build', *needles)
        # At partition time too, before the generic absent-identity guard could name it.
        self.refusal({feature_only: self.entry()}, *needles, inventory=self.inventory())
        self.assertNotIn('not in the test inventory', str(self.held_refusal(feature_only)))
        trace = 'review-core perf_trace::tests::a_recorder_test'
        self.refusal({trace: self.entry()}, '.config/test-quarantine.json: ' + trace,
                     'trace feature build', *needles)

    def held_refusal(self, identity):
        self.held_file({identity: self.entry()})
        with self.assertRaises(ValueError) as caught:
            tc.partition(self.inventory())
        return caught.exception

    @staticmethod
    def excluded(arguments, binary, name):
        # The exclusion is one `not (...)` over exact `binary_id` and `test` equalities.
        expression = arguments[arguments.index('-E') + 1]
        held = tc.re.findall(r'\(binary_id\(=([^)]+)\) & test\(=([^)]+)\)\)', expression)
        assert expression == 'not (' + ' | '.join(f'(binary_id(={b}) & test(={n}))' for b, n in held) + ')'
        return (binary, name) in held

    def test_the_full_gate_route_runs_nextest_without_the_quarantined_test(self):
        # make check (verify.sh --profile full) reaches nextest only through the `test` recipe.
        dry = subprocess.run(['make', '-n', 'check', 'TEST_THREADS=4'], cwd=ROOT, capture_output=True,
                             text=True, check=True).stdout
        route = 'scripts/test-classes.py nextest-gate -- --locked --profile ci --test-threads 4'
        self.assertIn(route, dry)
        self.assertNotIn('scripts/nextest-gate.py', dry)  # No other, unfiltered nextest run.
        argv = ['test-classes.py', *route.split(' ')[1:]]
        caller = argv[argv.index('--') + 1:]

        def run(tests):
            self.held_file(tests)
            with mock.patch.object(sys, 'argv', argv), mock.patch.object(tc.os, 'execv') as execv:
                self.assertEqual(tc.main(), 0)
            executable, command = execv.call_args.args
            self.assertEqual(command[:2], [executable, str(ROOT / 'scripts/nextest-gate.py')])
            return command[2:]

        self.assertEqual(run({}), caller)  # An empty quarantine changes nothing.
        sibling = 'af::it cli_surface::help_lists_every_command'
        arguments = run({self.held: self.entry()})
        self.assertEqual(arguments[:-2], caller)
        self.assertTrue(self.excluded(arguments, *self.held.split(' ')))
        self.assertFalse(self.excluded(arguments, *sibling.split(' ')))
        with self.assertRaisesRegex(ValueError, 'only filter; fix: '):
            tc.gate_arguments([*caller, '-E', 'all()'], {self.held: self.entry()})
        self.held_file({self.held: self.entry(since='2026-10-01', expires='2026-10-10')})
        with mock.patch.object(sys, 'argv', argv), mock.patch.object(tc.os, 'execv') as execv, \
                self.assertRaisesRegex(ValueError, 'expired on 2026-10-10'):
            tc.main()
        execv.assert_not_called()


class Gates(unittest.TestCase):
    def good(self, plan):
        return {'resolve': {'result': 'success', 'outputs': {'plan': json.dumps(plan, separators=(',', ':'))}},
                'base': {'result': 'success'}, 'classes': {'result': 'success'},
                'container-probes': {'result': 'success' if 'e2e' in plan['selected'] else 'skipped'},
                'telemetry-build-smoke': {'result': 'success' if 'perf' in plan['selected'] else 'skipped'},
                'telemetry-runtime-smoke': {'result': 'success' if 'perf' in plan['selected'] else 'skipped'}}

    def test_all_required_results_fail_closed(self):
        plan = tc.resolve(['release:full'], 'pr')
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

    def test_telemetry_smoke_requires_actual_success_only_when_perf_selected(self):
        plans = [tc.resolve(labels, 'pr') for labels in
                 ([], ['no-perf'], ['release:fast'], ['test:perf'], ['release:full'])]
        plans += [tc.resolve(context=context) for context in ('local', 'gate', 'release_pr', 'release')]
        for plan in plans:
            expected = 'success' if 'perf' in plan['selected'] else 'skipped'
            for job in ('telemetry-build-smoke', 'telemetry-runtime-smoke'):
                for state in ('success', 'skipped', 'failure', 'cancelled', ''):
                    needs = self.good(plan)
                    needs[job]['result'] = state
                    with self.subTest(plan=plan, job=job, state=state):
                        if state == expected:
                            tc.aggregate(plan, needs)
                        else:
                            with self.assertRaises(ValueError):
                                tc.aggregate(plan, needs)
                needs = self.good(plan)
                del needs[job]
                with self.assertRaises(ValueError):
                    tc.aggregate(plan, needs)

    def test_selected_platform_receipts_are_exact_and_complete(self):
        plan = tc.resolve(['test:uix'])
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with self.assertRaises(ValueError):
                tc.receipts(plan, root, ['Linux', 'macOS'])
            receipt = {'schema': 'af.test-class-receipt/2', 'plan': plan,
                       'platform': 'Linux', 'class': 'uix', 'status': 'success'}
            tc.write(root / 'linux/receipt.json', receipt)
            tc.receipts(plan, root, ['Linux'])
            for mutation in [{'status': 'cancelled'}, {'class': 'perf'}, {'unexpected': 1},
                             {'schema': 'af.test-class-receipt/1'},
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
    def test_cli_report_and_aggregate_keep_build_evidence_out_of_class_totals(self):
        plan = tc.resolve(['release:full'], 'pr')
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tc.write(root / 'plan.json', plan)
            tc.write(root / 'needs.json', Gates().good(plan))
            env = dict(os.environ)
            env.pop('GITHUB_STEP_SUMMARY', None)

            def cli(*args):
                return subprocess.run([sys.executable, str(ROOT / 'scripts/test-classes.py'),
                                       *args], cwd=root, env=env, capture_output=True, text=True)

            def metric(label, seconds, stage='tests'):
                return {'schema': 'af.ci-step/1', 'stage': stage, 'label': label,
                        'elapsed_seconds': seconds, 'exit_code': 0,
                        'started_at': '2026-10-27T00:00:00+00:00'}

            def steps(path, records):
                path.write_text('\n'.join(map(json.dumps, records)) + '\n')

            def report(path, output):
                return cli('report', '--plan', 'plan.json', '--steps', str(path),
                           '--source-run', 'synthetic-cli-regression', '--source-sha', 'none',
                           '--output', str(output))

            def aggregate():
                return cli('aggregate', '--plan', 'plan.json', '--needs', 'needs.json',
                           '--evidence', str(root / 'artifacts'), '--platforms', 'Linux')

            combined = []
            for kind, seconds in zip(tc.CLASSES, (5, 0, 7, 11)):
                artifact = root / 'artifacts' / ('validation-class-Linux-' + kind)
                evidence = artifact / 'class-evidence'
                tc.write(evidence / 'receipt.json', {'schema': 'af.test-class-receipt/2',
                         'plan': plan, 'platform': 'Linux', 'class': kind, 'status': 'success'})
                path = artifact / 'class-metrics.jsonl'
                output = evidence / 'report.json'
                inner = [metric('class-build', 2, 'build'), metric('class-' + kind + '-inner', 1)]
                # Hosted run reports before the outer wrapper has written its own receipt.
                steps(path, inner)
                result = report(path, output)
                self.assertEqual(result.returncode, 0, result.stderr)
                value = tc.read(output)
                self.assertEqual(value['ci_steps'], inner)
                self.assertIsNone(value['classes'][kind]['step_seconds'])
                self.assertIsNone(value['class_runner_seconds'])
                self.assertIsNone(value['parallel_class_critical_path_seconds'])
                records = inner + [metric('class-' + kind, seconds)]
                combined.extend(records)
                steps(path, records)
                result = report(path, output)
                self.assertEqual(result.returncode, 0, result.stderr)
                value = tc.read(output)
                self.assertEqual(value['ci_steps'], records)
                self.assertEqual(set(value['classes']), set(tc.CLASSES))
                self.assertEqual(value['classes'][kind]['step_seconds'], seconds)
                self.assertIsNone(value['class_runner_seconds'])  # Other selected jobs are absent.

            steps(root / 'combined.jsonl', combined)
            result = report(root / 'combined.jsonl', root / 'combined.json')
            self.assertEqual(result.returncode, 0, result.stderr)
            value = tc.read(root / 'combined.json')
            self.assertEqual(value['ci_steps'], combined)
            self.assertEqual(value['class_runner_seconds'], 23)
            self.assertEqual(value['parallel_class_critical_path_seconds'], 11)

            result = aggregate()
            self.assertEqual(result.returncode, 0, result.stderr)
            value = tc.read(root / 'target/test-classes/validation-metrics.json')['platforms']['Linux']
            self.assertEqual(value['observed_runner_command_seconds'], 23)
            self.assertEqual(value['parallel_class_critical_path_seconds'], 11)
            self.assertEqual(value['observed_command_envelope_seconds'], 11)
            self.assertEqual(value['classes']['e2e']['command_seconds'], 0)
            self.assertEqual(value['classes']['e2e']['runner_cost_share'], 0)
            self.assertAlmostEqual(value['classes']['functional']['runner_cost_share'], 5 / 23)

            artifact = root / 'artifacts/validation-class-Linux-functional'
            path = artifact / 'class-metrics.jsonl'
            output = artifact / 'class-evidence/report.json'
            inner = [metric('class-build', 2, 'build')]
            for invalid in (-1, 'NaN', 'Infinity'):
                with self.subTest(invalid=invalid):
                    steps(path, inner + [metric('class-functional', invalid)])
                    result = report(path, output)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn('invalid duration', result.stderr)
                    result = aggregate()
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn('invalid duration', result.stderr)
            for records in (inner, inner + [metric('class-functional', 5)] * 2,
                            inner + [dict(metric('class-functional', 5), exit_code=1)]):
                steps(path, records)
                result = aggregate()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('missing/failed outer class metric', result.stderr)
            steps(path, inner + [metric('class-functional', 5)])
            (artifact / 'class-evidence/receipt.json').unlink()
            result = aggregate()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('missing class receipts', result.stderr)

    def test_perf_smoke_cost_is_separate_but_extends_observed_envelope(self):
        plan = tc.resolve(['test:perf'])
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            artifact = root / 'validation-class-Linux-perf-1'
            evidence = artifact / 'class-evidence'
            tc.write(evidence / 'receipt.json', {'platform': 'Linux', 'class': 'perf'})
            tc.write(evidence / 'report.json', {'plan': plan, 'classes': {'perf': {'passed': 1}}})
            outer = {'label': 'class-perf', 'started_at': '2026-10-25T00:00:00+00:00',
                     'elapsed_seconds': 5, 'exit_code': 0}
            (artifact / 'class-metrics.jsonl').write_text(json.dumps(outer) + '\n')
            smoke = root / 'validation-telemetry-smoke-1'
            smoke.mkdir()
            (smoke / 'telemetry-smoke-metrics.jsonl').write_text(json.dumps(dict(
                outer, label='telemetry-measurement-smoke', elapsed_seconds=20)) + '\n')
            runtime = root / 'validation-telemetry-runtime-smoke-1'
            runtime.mkdir()
            (runtime / 'telemetry-runtime-smoke-metrics.jsonl').write_text(json.dumps(dict(
                outer, label='telemetry-runtime-smoke', elapsed_seconds=30)) + '\n')
            value = tc.aggregate_metrics(plan, root, ['Linux'])['platforms']['Linux']
            self.assertEqual(value['observed_runner_command_seconds'], 5)
            self.assertEqual(value['classes']['perf']['runner_cost_share'], 1)
            self.assertEqual(value['perf_smoke'], {'state': 'RAN', 'command_seconds': 50})
            self.assertEqual(value['observed_command_envelope_seconds'], 30)
            self.assertEqual(value['last_finishing_command'], 'telemetry-runtime-smoke')

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
                tc.write(evidence / 'receipt.json', {'schema': 'af.test-class-receipt/2', 'plan': plan,
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
