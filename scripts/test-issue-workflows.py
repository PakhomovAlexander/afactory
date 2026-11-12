#!/usr/bin/env python3
"""Hermetic issue Workflow routing, composition, history and plan refusal tests."""
import contextlib
import copy
import io
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('workflows', ROOT / 'scripts/issue-workflows.py')
w = importlib.util.module_from_spec(spec)
spec.loader.exec_module(w)


def pin(pipeline, kind='implement'):
    return {'schema': 'af/scenario-template-pin@1', 'template_id': 'sha256:' + '1' * 64,
            'template': {'schema': 'af/scenario-task-template@1', 'repo': '/project', 'state': '/private/state',
                         'authority_commit': '1' * 40, 'repository_id': 'fixture',
                         'source_content_digest': 'sha256:' + '2' * 64, 'authority_id': 'sha256:' + '3' * 64,
                         'max_tokens': 1000, 'bindings': None,
                         'document': {'schema': 'af.task-file/1', 'task_id': 'placeholder', 'kind': kind,
                                      'goal': 'Bounded issue fixture', 'requirements': {'acceptance': ['exact']}, 'base': {'commit': '1' * 40}, 'pipeline': {'name': pipeline, 'fallback': 'refuse'},
                                      'limits': {'tokens': 1000, 'wall_ms': 1000}}}}


def research_requirements(goal='Bounded issue fixture', requirements=None):
    return {'acceptance': ['diagnose or design; do not claim implementation'],
            'original_issue': w.original_issue(goal, requirements or {'acceptance': ['exact']})}


def check_selection(name, value):
    if value['selection']['assessment']['decision'] != {'kind': 'selected', 'pipeline': name}:
        raise ValueError('wrong selection')


def pilot(name):
    choice = w.selection(name)
    manifest = {'schema': 'af.issue-workflow-pilot/1', 'issue': 'https://github.com/o/r/issues/1',
                'pull_request': None, 'source_commit': '1' * 40, 'selection': name,
                'tasks': ['original', 'repair'], 'verification_task': 'verify',
                'research_tasks': ['research'] if choice['research_pipeline'] else []}
    origin = {'schema': 'af.task-source-origin/3', 'kind': 'captured', 'repository_id': 'root',
              'source_revision': '1' * 40, 'content_digest': 'digest'}
    artifacts = {'origin': origin, 'source': {'schema': 'af.task-snapshot/1', 'origin_id': 'origin'}}
    values = {}
    for task in ['original', 'repair', 'verify'] + manifest['research_tasks']:
        pipeline = (w.catalog()['verification'] if task == 'verify' else choice['research_pipeline']
                    if task == 'research' else name)
        source = 'repair-source' if task == 'repair' else 'verify-source' if task == 'verify' else 'source'
        inputs = {'source': {'snapshot_id': source},
                  'requirements': {'artifact_type': 'af/Requirements@1', 'artifact_ids': [task + '-requirements']}}
        artifacts[task + '-requirements'] = {'type': 'af/Requirements@1', 'payload': {'specification': {'acceptance': ['exact'], 'issue_url': manifest['issue']}}}
        if task == 'research':
            artifacts[task + '-requirements']['payload']['specification'] = research_requirements('issue goal', {'acceptance': ['exact'], 'issue_url': manifest['issue']})
        artifacts[task + '-revision'] = {'type': 'af/TaskRevision@1', 'payload': {
            'task_id': task, 'goal': 'issue goal' if task != 'research' else 'research goal',
            'inputs': inputs, 'provenance': {'input_artifact_ids': []}}}
        values[task] = {'schema': 'af/task-inspection@20', 'task_id': task, 'revision_id': task + '-revision',
                        'plan_id': 'plan-' + task, 'chargeable_tokens': '10', 'state': {},
                        'unknown_usage_attempts': 1 if task in ('repair', 'research') else 0,
                        'selection': {'assessment': {'decision': {'kind': 'selected', 'pipeline': pipeline}}},
                        'plan': {'inputs': inputs, 'dependencies': {pipeline: {}}},
                        'phase': {'kind': 'finished', 'result_id': task + '-result'},
                        'result': {'acceptance': 'satisfied', 'execution': 'completed', 'outputs': {
                            'report': {'artifact_type': 'af/Document@1', 'artifact_ids': [task + '-report']},
                            'snapshot': {'artifact_type': 'af/SourceTree@1', 'artifact_ids': [task + '-tree'], 'snapshot_id': task + '-candidate'}}}}
    def bind(consumer, producer, port, root):
        output = copy.deepcopy(values[producer]['result']['outputs'][port])
        inputs = values[consumer]['plan']['inputs']
        inputs[root] = output
        binding = {'artifact_id': output['artifact_ids'][0], 'task': {
            'task_id': producer, 'task_revision_id': producer + '-revision', 'result_id': producer + '-result',
            'port': port, 'acceptance': 'satisfied'}}
        if port == 'snapshot':
            binding['snapshot_id'] = output['snapshot_id']
        values[consumer].setdefault('input_bindings', {'bindings': {}})['bindings'][root] = binding
    if choice['research_pipeline']:
        for task in manifest['tasks']:
            bind(task, 'research', 'report', 'research_report')
        artifacts['research-origin'] = copy.deepcopy(origin)
        artifacts['source']['origin_id'] = 'research-origin'
        # Keep the original origin independently available for wrong-repository tests.
        values['original']['plan']['inputs']['source']['snapshot_id'] = 'original-source'
        artifacts['original-source'] = {'schema': 'af.task-snapshot/1', 'origin_id': 'origin'}
        # Native sources must be the same Snapshot, not merely equal origins.
        values['research']['plan']['inputs']['source']['snapshot_id'] = 'original-source'
    artifacts['repair-source'] = {'schema': 'af.task-snapshot/1', 'origin_id': 'repair-origin'}
    artifacts['repair-origin'] = {'schema': 'af.task-source-origin/3', 'kind': 'bound', 'repository_id': 'root',
                                  'bound_from': {'snapshot_id': 'original-candidate', 'task': {
                                      'task_id': 'original', 'task_revision_id': 'original-revision'}}}
    values['repair']['continues'] = {'task_id': 'original', 'snapshot_id': 'original-candidate'}
    bind('verify', 'repair', 'snapshot', 'source')
    artifacts['base'] = {'type': 'af/TaskBase@1', 'payload': {'commit': '1' * 40, 'snapshot_id': 'source' if not choice['research_pipeline'] else 'original-source'}}
    artifacts['verify-revision']['payload']['provenance']['input_artifact_ids'] = ['base']
    return manifest, values, artifacts


REAL_CAPTURED_CATALOG = w.captured_catalog

FACTS = dict(w.read(ROOT / 'fixtures/issue-workflows/routing-known.json'), specified_requirements=True, deterministic_acceptance=True, known_bottleneck=True, measured_reproduction=True)


class DirectPlanning(unittest.TestCase):
    def plan(self, task):
        with tempfile.TemporaryDirectory() as directory:
            task_file = Path(directory) / 'task.json'
            routing_file = Path(directory) / 'routing.json'
            task_file.write_text(json.dumps(task))
            routing_file.write_text(json.dumps(FACTS))
            output = io.StringIO()
            with mock.patch.object(sys, 'argv', ['issue-workflows.py', '--state', '/private/state',
                                                'plan', task['pipeline']['name'], '--file', str(task_file),
                                                '--routing', str(routing_file)]), \
                 mock.patch.object(w, 'installed_authority'), \
                 mock.patch.object(w, 'check_latest_models'), \
                 mock.patch.object(w, 'validate_plan', side_effect=lambda name, value, state: value), \
                 contextlib.redirect_stdout(output):
                w.main()
            return json.loads(output.getvalue())

    def test_direct_design_refuses_missing_or_malformed_original_issue_before_native_planning(self):
        task = pin('design/astra-kimi', 'report')['template']['document']
        valid = research_requirements()
        invalid = [{}, None, [], dict(valid, original_issue=None), dict(valid, original_issue=[])]
        for field in ('goal', 'requirements', 'acceptance'):
            requirements = copy.deepcopy(valid)
            del requirements['original_issue'][field]
            invalid.append(requirements)
        for field, value in [('goal', ''), ('goal', 7), ('requirements', []),
                             ('requirements', {}), ('requirements', {'acceptance': []}),
                             ('requirements', {'acceptance': 'exact'}),
                             ('requirements', {'acceptance': ['']}),
                             ('requirements', {'acceptance': [False]})]:
            requirements = copy.deepcopy(valid)
            requirements['original_issue'][field] = value
            invalid.append(requirements)
        requirements = copy.deepcopy(valid)
        requirements['original_issue']['extra'] = 'unknown'
        invalid.append(requirements)
        for requirements in invalid:
            with self.subTest(requirements=requirements), mock.patch.object(w, 'invoke', return_value={}) as invoke:
                task['requirements'] = requirements
                with self.assertRaisesRegex(ValueError, 'original issue|manifest fields'):
                    self.plan(task)
                invoke.assert_not_called()
        del task['requirements']
        with mock.patch.object(w, 'invoke', return_value={}) as invoke, \
             self.assertRaisesRegex(ValueError, 'original issue'):
            self.plan(task)
        invoke.assert_not_called()

    def test_direct_design_with_original_issue_still_plans(self):
        task = pin('design/astra-kimi', 'report')['template']['document']
        task['requirements'] = research_requirements()
        with mock.patch.object(w, 'invoke', return_value={'plan_id': 'native-plan'}) as invoke:
            self.assertEqual(self.plan(task), {'plan_id': 'native-plan'})
        self.assertEqual(invoke.call_count, 1)
        self.assertEqual(invoke.call_args.args[3:5], ('plan', '--file'))

    def test_other_direct_plans_do_not_require_original_issue(self):
        task = pin('bugfix/default-codex')['template']['document']
        with mock.patch.object(w, 'invoke', return_value={'plan_id': 'native-plan'}) as invoke:
            self.assertEqual(self.plan(task), {'plan_id': 'native-plan'})
        self.assertEqual(invoke.call_count, 1)


class Workflows(unittest.TestCase):
    def setUp(self):
        patcher = mock.patch.object(w, 'captured_catalog')
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_latest_models_per_family_and_fail_closed_catalogs(self):
        models = [dict(id=m, effort_levels=['medium', 'high']) for m in
                  ('gpt-6.1-sol', 'gpt-6.1-terra', 'gpt-6-astra', 'gpt-6-sol',
                   'gpt-5.6-sol', 'k3.1-pro', 'k99-pro-highspeed', 'gpt-99-luna')]
        with mock.patch.object(w, 'available_models', return_value=models):
            for name in w.catalog()['selections']:
                w.check_latest_models('af', name)
        for model, selection in [('gpt-6.2-sol', 'bugfix/default-codex'),
                                 ('gpt-6.10-terra', 'bugfix/light-codex'),
                                 ('gpt-6.1-astra', 'design/astra-kimi'),
                                 ('k3.2-pro', 'issue-internal/kimi-report'),
                                 ('gpt-6.1.0-sol', 'bugfix/default-codex')]:
            changed = models + [dict(id=model, effort_levels=['medium', 'high'])]
            with mock.patch.object(w, 'available_models', return_value=changed), self.assertRaises(ValueError):
                w.check_latest_models('af', selection)
        for changed in ([], [m for m in models if m['id'] != 'gpt-6-astra'],
                        [m for m in models if m['id'] != 'gpt-6.1-terra'],
                        [dict(m, effort_levels=['medium']) if m['id'] == 'gpt-6.1-terra' else m for m in models],
                        [dict(m, effort_levels=[]) for m in models]):
            with mock.patch.object(w, 'available_models', return_value=changed), self.assertRaises(ValueError):
                w.check_latest_models('af', 'bugfix/default-codex')
        self.assertEqual(w.model_family('gpt-6.10-sol')[1], (6, 10))
        self.assertIsNone(w.model_family('k3.1-pro-highspeed'))

    def test_catalog_protocol_refuses_unavailable_duplicate_and_incomplete_pages(self):
        row = dict(id='pi-personal', kind='pi', state='listed', skipped_entries=0,
                   models=[dict(id='k3.1-pro', effort_levels=['high'])],
                   catalog_page=dict(number=0, next=None, total=1))
        value = dict(schema='af/provider-models@4', exit_code=0, token_free=True, providers=[row])
        def response(v, code=0):
            return mock.Mock(returncode=code, stdout=json.dumps(v).encode())
        with mock.patch.object(w.subprocess, 'run', return_value=response(value)) as run:
            self.assertEqual(w.available_models('af', 'pi-personal'), row['models'])
            self.assertEqual(run.call_args.args[0], ['af', 'provider', 'models', 'pi-personal', '--json', '--page', '0'])
        changes = [lambda v: v.update(token_free=False), lambda v: v.update(exit_code=7),
                   lambda v: v['providers'].append(copy.deepcopy(row)),
                   lambda v: v['providers'][0].update(state='unavailable'),
                   lambda v: v['providers'][0].update(skipped_entries=1),
                   lambda v: v['providers'][0].update(id='other'),
                   lambda v: v['providers'][0]['catalog_page'].update(total=2),
                   lambda v: v['providers'][0]['models'].append(copy.deepcopy(row['models'][0]))]
        for change in changes:
            changed = copy.deepcopy(value); change(changed)
            with mock.patch.object(w.subprocess, 'run', return_value=response(changed)), self.assertRaises(ValueError):
                w.available_models('af', 'pi-personal')
        page0 = copy.deepcopy(value); page0['providers'][0]['catalog_page'].update(next=1, total=2)
        page1 = copy.deepcopy(value); page1['providers'][0]['catalog_page'].update(number=1, total=2)
        page1['providers'][0]['models'][0]['id'] = 'k3-pro'
        with mock.patch.object(w.subprocess, 'run', side_effect=[response(page0), response(page1)]):
            self.assertEqual(len(w.available_models('af', 'pi-personal')), 2)
        with mock.patch.object(w.subprocess, 'run', return_value=response(value, 7)), self.assertRaises(ValueError):
            w.available_models('af', 'pi-personal')

    def test_light_implementation_and_small_bottleneck_routing(self):
        facts = dict(FACTS, known_cause=False, deterministic_regression=False)
        w.route('impl/light-kimi-codex', facts)
        w.route('impl/light-kimi-codex', {k: v for k, v in facts.items() if k not in ('known_cause', 'deterministic_regression')})
        w.route('perf/research-kimi-codex', dict(facts, known_cause=True))
        for name, keys in [('impl/light-kimi-codex', ('specified_requirements', 'deterministic_acceptance', 'narrow_change')),
                           ('perf/research-kimi-codex', ('known_cause', 'known_bottleneck', 'measured_reproduction', 'narrow_change'))]:
            for key in keys:
                with self.assertRaises(ValueError):
                    w.route(name, dict(FACTS, **{key: False}))
            for key in ('authority', 'contract', 'security', 'accounting', 'persistence', 'uncertain_concurrency'):
                with self.assertRaises(ValueError):
                    w.route(name, dict(FACTS, **{key: True}))

    def test_design_only_reconciliation_preserves_source_issue_and_cost(self):
        manifest, values, artifacts = pilot('design/astra-kimi')
        manifest.update(tasks=['original'], research_tasks=[])
        del manifest['verification_task']
        artifacts['original-requirements']['payload']['specification'] = research_requirements(
            'issue goal', {'acceptance': ['exact'], 'issue_url': manifest['issue']})
        def reconcile():
            with mock.patch.object(w, 'invoke', side_effect=lambda *a: values[a[-1]]), \
                 mock.patch.object(w, 'recorded', side_effect=lambda state, identity, kind=None:
                                   artifacts[identity]['payload'] if kind else artifacts[identity]), \
                 mock.patch.object(w, 'validate_plan', side_effect=lambda n, v, state: check_selection(n, v)), \
                 mock.patch.object(w.subprocess, 'check_output', return_value=b'root\n'):
                return w.reconcile('af', '/project', '/state', manifest)
        result = reconcile()
        self.assertEqual(result['chain_chargeable_tokens'], '10')
        self.assertFalse(result['final_verification_recorded'])
        self.assertEqual(result['tasks'][0]['revision_id'], 'original-revision')
        for field, bad in [('acceptance', 'unsatisfied'), ('execution', 'failed')]:
            original = values['original']['result'][field]
            values['original']['result'][field] = bad
            with self.assertRaises(ValueError):
                reconcile()
            values['original']['result'][field] = original
        manifest['tasks'].append('repair')
        with self.assertRaises(ValueError):
            reconcile()
        manifest['tasks'].pop()
        artifacts['origin']['source_revision'] = '2' * 40
        with self.assertRaisesRegex(ValueError, 'source commit or repository'):
            reconcile()

    def test_design_reuse_requires_original_issue_and_exact_native_stage(self):
        pins = [pin('design/astra-kimi', 'report'), pin('impl/default-codex'),
                pin(w.catalog()['verification'], 'verification')]
        pins[0]['template']['document']['requirements'] = research_requirements()
        result = w.composition('impl/default-codex', pins, FACTS)['definitions']['issue-workflow']
        self.assertEqual(result['steps'][1]['inputs']['research_report']['step'], 'research')
        pins[0]['template']['document']['requirements']['original_issue']['goal'] = 'unrelated'
        with self.assertRaisesRegex(ValueError, 'original issue contract'):
            w.composition('impl/default-codex', pins, FACTS)

    def test_installed_and_captured_authority_pin_both_real_policies(self):
        staged = w.STAGED
        for kind in ('code', 'report'):
            self.assertEqual((staged / ('approved-' + kind + '-policy.toml')).read_bytes(),
                             (ROOT / ('.af/' + kind + '-policy.toml')).read_bytes())
        files = {'af.toml': 'replacement-af.toml',
                 'task-catalog.toml': 'replacement-catalog.toml',
                 'code-policy.toml': 'approved-code-policy.toml',
                 'report-policy.toml': 'approved-report-policy.toml'}
        def capture(args, **kwargs):
            file = args[-1].split(':.af/')[1]
            return mock.Mock(stdout=(staged / files[file]).read_bytes())
        # This test deliberately calls the real production guard, unlike generic Scenario fixtures.
        with mock.patch.object(w.subprocess, 'run', side_effect=capture):
            REAL_CAPTURED_CATALOG(pin('bugfix/default-codex')['template'])
        for file in files:
            def changed(args, **kwargs):
                result = capture(args, **kwargs)
                if args[-1].endswith('/' + file):
                    result.stdout = b'changed'
                return result
            with self.subTest(file=file), mock.patch.object(w.subprocess, 'run', side_effect=changed):
                with self.assertRaises(ValueError):
                    REAL_CAPTURED_CATALOG(pin('bugfix/default-codex')['template'])

    def test_installed_settings_refuse_retired_default_and_limit_drift(self):
        with tempfile.TemporaryDirectory() as directory:
            authority = Path(directory) / '.af'
            authority.mkdir()
            for installed, staged in [('af.toml', 'replacement-af.toml'),
                                      ('task-catalog.toml', 'replacement-catalog.toml'),
                                      ('code-policy.toml', 'approved-code-policy.toml'),
                                      ('report-policy.toml', 'approved-report-policy.toml')]:
                (authority / installed).write_bytes((w.STAGED / staged).read_bytes())
            w.installed_authority(directory)
            settings = (authority / 'af.toml').read_text()
            for changed in [settings.replace('bugfix/light-kimi-terra-sol', 'default/implementation'),
                            settings.replace('8000000', '8000001')]:
                (authority / 'af.toml').write_text(changed)
                with self.assertRaisesRegex(ValueError, 'af.toml'):
                    w.installed_authority(directory)

    def test_performance_preview_follows_composed_tasks_and_pinned_dataflow(self):
        name = 'perf/research-experiment-codex'
        choice = w.selection(name)
        pins = [pin(choice['research_pipeline'], 'report'), pin(name),
                pin(w.catalog()['verification'], 'verification')]
        pins[0]['template']['document']['requirements'] = research_requirements()
        flow = w.composition(name, pins, FACTS)['definitions']['issue-workflow']['steps']
        self.assertEqual([step['id'] for step in flow], ['research', 'implement', 'verify'])
        self.assertEqual(flow[1]['inputs']['research_report']['step'], 'research')
        self.assertEqual(flow[2]['inputs']['source']['step'], 'implement')
        pipeline = w.toml(w.STAGED / 'task-packages' / name / 'pipeline.toml')
        nodes = {node['id']: node for node in pipeline['nodes']}
        self.assertEqual(nodes['baseline']['operator']['op'], 'measure')
        self.assertEqual(nodes['review']['operator']['pipeline'], 'issue-internal/review')
        self.assertEqual(nodes['trial']['inputs']['baseline']['node'], 'baseline')
        self.assertEqual(nodes['trial']['inputs']['source']['node'], 'accept_goal')
        self.assertEqual(nodes['accept_measurement']['inputs']['evaluation']['node'], 'trial')
        self.assertEqual(choice['stages'], ['research', 'baseline', 'implementation', 'code review',
                                           'measurement', 'comparison', 'measurement acceptance',
                                           'verification'])

    def test_effective_policy_identity_review_binding_and_local_execution(self):
        revision = {'authority': {'policy_id': 'authority'}}
        catalog_bytes = (w.STAGED / 'replacement-catalog.toml').read_bytes()
        catalog_id = w.kernel_contracts.kernel_blob_id(catalog_bytes)
        review = w.approved_review_policy()
        review_id = w.kernel_contracts.kernel_digest(w.kernel_contracts.KERNEL_CONTENT_DOMAIN,
                                                    review, 'review')
        for kind in ('code', 'report'):
            policies = {kind + '_policy_id': w.approved_policy_id(kind)}
            if kind == 'code':
                policies['review_policy_id'] = review_id
            authority = dict(policies, catalog_id=catalog_id)
            value = {'revision_id': 'revision', 'plan': {'authority': {'policy_id': 'authority'}},
                     'selection': {'assessment': {'decision': {'pipeline':
                         'issue-internal/report' if kind == 'report' else 'bugfix/default-codex'}}},
                     'graph': {'nodes': {}}}
            records = {'revision': revision, 'authority': authority, review_id: review,
                       w.approved_policy_id(kind): {}}
            with mock.patch.object(w, 'recorded', side_effect=lambda state, identity, *a: records[identity]), \
                 mock.patch.object(w, 'recorded_bytes', return_value=catalog_bytes):
                w.validate_policy(value, '/state')
                # Every policy Option must be exactly present or absent as native capture writes it.
                for key in ('code_policy_id', 'document_policy_id', 'report_policy_id', 'review_policy_id'):
                    for substitute in ('absent', None, 'wrong'):
                        if substitute == 'absent' and key not in policies:
                            continue
                        authority.pop(key, None)
                        if substitute != 'absent':
                            authority[key] = substitute
                        with self.subTest(kind=kind, key=key, substitute=substitute), \
                             self.assertRaisesRegex(ValueError, 'policy identity'):
                            w.validate_policy(value, '/state')
                        authority.pop(key, None)
                        authority.update(policies)
                for substitute in (None, 'wrong'):
                    authority['catalog_id'] = substitute
                    with self.assertRaisesRegex(ValueError, 'catalog identity'):
                        w.validate_policy(value, '/state')
                del authority['catalog_id']
                with self.assertRaisesRegex(ValueError, 'catalog identity'):
                    w.validate_policy(value, '/state')
                authority['catalog_id'] = catalog_id
                if kind == 'code':
                    for field in review:
                        altered = dict(review)
                        altered[field] = None
                        records[review_id] = altered
                        with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'Review policy'):
                            w.validate_policy(value, '/state')
                    records[review_id] = review
                value['graph']['nodes']['gate'] = {'operator': {'operator': {'remote_checks': ['kernel']}}}
                with self.assertRaisesRegex(ValueError, 'locally'):
                    w.validate_policy(value, '/state')
                value['graph']['nodes'] = {}
                value['plan']['authority']['policy_id'] = 'forged'
                with self.assertRaisesRegex(ValueError, 'native revision'):
                    w.validate_policy(value, '/state')

    def test_catalog_blob_requires_exact_addressed_bytes(self):
        data = (w.STAGED / 'replacement-catalog.toml').read_bytes()
        identity = w.kernel_contracts.kernel_blob_id(data)
        with tempfile.TemporaryDirectory() as state:
            path = Path(state) / 'cas/objects' / identity[7:9] / identity[9:]
            path.parent.mkdir(parents=True)
            path.write_bytes(data)
            self.assertEqual(w.recorded_bytes(state, identity), data)
            path.write_bytes(data.replace(b'"major"', b'"blocker"'))
            with self.assertRaisesRegex(ValueError, 'blob identity'):
                w.recorded_bytes(state, identity)

    def test_all_nine_are_composed_from_native_tasks_with_no_actions(self):
        for name, selection in w.catalog()['selections'].items():
            research = selection['research_pipeline']
            pins = ([pin(research, 'report')] if research else []) + [pin(name), pin(w.catalog()['verification'], 'verification')]
            if research:
                pins[0]['template']['document']['goal'] = 'Distinct research question'
                pins[0]['template']['document']['requirements'] = research_requirements()
            if name == 'design/astra-kimi':
                pins = [pin(name, 'report')]
                pins[0]['template']['document']['requirements'] = research_requirements()
            composed = w.composition(name, pins, FACTS)['definitions']['issue-workflow']
            self.assertEqual(composed['authority']['grants'], [])
            self.assertEqual(composed['authority']['decision_owners'], [])
            steps = {s['id']: s for s in composed['steps']}
            if name == 'design/astra-kimi':
                self.assertEqual(list(steps), ['design'])
                continue
            self.assertEqual(steps['verify']['operation']['bindings']['source']['port'], 'snapshot')
            self.assertEqual(steps['implement']['max_attempts'], 1)
            if research:
                self.assertEqual(steps['implement']['operation']['bindings']['research_report'],
                                 {'port': 'report', 'artifact_type': 'af/Document@1'})
                self.assertEqual(steps['implement']['inputs']['research_report']['step'], 'research')
                with self.assertRaises(ValueError):
                    w.composition(name, pins[1:], FACTS)
                pins[0]['template']['source_content_digest'] = 'sha256:' + '4' * 64
                with self.assertRaises(ValueError):
                    w.composition(name, pins, FACTS)

    def test_research_and_design_require_the_actual_implementation_contract(self):
        for name in ('bugfix/research-fix-codex', 'perf/research-kimi-codex'):
            stage = w.selection(name)['research_pipeline']
            for change in ('missing', 'empty', 'goal', 'acceptance', 'type', 'extra', 'top-acceptance'):
                pins = [pin(stage, 'report'), pin(name), pin(w.catalog()['verification'], 'verification')]
                report = pins[0]['template']['document']
                report['goal'] = 'A distinct stage objective'
                report['requirements'] = research_requirements()
                contract = report['requirements']['original_issue']
                if change == 'missing':
                    del report['requirements']['original_issue']
                elif change == 'empty':
                    contract['requirements'] = {}
                elif change == 'goal':
                    contract['goal'] = 'Another issue on the same source'
                elif change == 'acceptance':
                    contract['requirements']['acceptance'] = ['unrelated']
                elif change == 'type':
                    contract['requirements']['extra'] = True
                    for p in pins[1:]:
                        p['template']['document']['requirements']['extra'] = 1
                elif change == 'extra':
                    contract['label'] = 'same shared URL'
                else:
                    contract['acceptance'] = [{'id': 'other', 'text': 'unrelated'}]
                with self.subTest(selection=name, change=change), self.assertRaises(ValueError):
                    w.composition(name, pins, FACTS)

    def test_examples_share_one_concrete_issue_but_distinct_stage_acceptance(self):
        root = ROOT / 'fixtures/issue-workflows'
        implementation = w.read(root / 'research-fix.json')
        expected = w.original_issue(implementation['goal'], implementation['requirements'])
        self.assertEqual(expected['requirements']['issue_url'], w.read(root / 'pilot.example.json')['issue'])
        for name in ('diagnose', 'design'):
            stage = w.read(root / (name + '.json'))
            w.check_research_issue(stage['requirements'], expected)
            self.assertNotEqual(stage['goal'], expected['goal'])
            self.assertNotEqual(stage['requirements']['acceptance'], expected['requirements']['acceptance'])
        for name in ('known-fix', 'verify'):
            task = w.read(root / (name + '.json'))
            self.assertEqual(w.original_issue(task['goal'], task['requirements']), expected)

    def test_composition_refuses_json_type_substitution(self):
        pins = [pin('bugfix/default-codex'), pin(w.catalog()['verification'], 'verification')]
        pins[0]['template']['document']['requirements'] = {'required': 1}
        pins[1]['template']['document']['requirements'] = {'required': True}
        with self.assertRaises(ValueError):
            w.composition('bugfix/default-codex', pins, FACTS)

    def test_shipped_task_examples_supply_their_pipeline_required_facts(self):
        examples = ROOT / 'fixtures/issue-workflows'
        files = []
        for file in sorted(examples.glob('*.json')):
            task = w.read(file)
            if task.get('schema') != 'af.task-file/1':
                continue
            files.append(file.name)
            with self.subTest(example=file.name):
                pipeline = w.toml(examples / 'task-packages' / task['pipeline']['name'] / 'pipeline.toml')
                self.assertIn(task['kind'], pipeline['accepts']['kinds'])
                for fact, value in pipeline['accepts']['required_facts'].items():
                    self.assertIn(fact, task.get('facts', {}))
                    self.assertEqual(task['facts'][fact], value)
        self.assertEqual(files, ['design.json', 'diagnose.json', 'known-fix.json', 'light-implementation.json',
                                 'performance-kimi.json', 'research-fix.json', 'research-kimi.json', 'verify.json'])

    def test_only_ordinary_default_drops_the_standard_selector_fact(self):
        staged = ROOT / 'fixtures/issue-workflows'
        contracts = w.read(staged / 'contracts.json')['pipelines']
        for name in w.catalog()['selections']:
            pipeline = w.toml(staged / 'task-packages' / name / 'pipeline.toml')
            facts = pipeline['accepts']['required_facts']
            self.assertEqual(facts, contracts[name]['accepts']['required_facts'])
            if name == 'bugfix/light-kimi-terra-sol':
                self.assertEqual(facts, {})
            elif '/light-' not in name and not name.startswith('design/'):
                self.assertIs(facts['standard'], True)

    def test_old_public_names_are_refused_without_aliases(self):
        for family in ('default', 'default-light', 'pi-light', 'warm', 'kernel', 'owner-default'):
            with self.assertRaises(ValueError):
                w.selection(family + '/implementation')
        self.assertEqual(len(w.catalog()['selections']), 9)
        with self.assertRaises(ValueError):
            w.selection('impl/design-codex')

    def test_light_scope_requires_all_explicit_narrow_facts_and_no_sensitive_work(self):
        facts = dict(known_cause=True, deterministic_regression=True, narrow_change=True,
                     authority=False, contract=False, security=False, accounting=False,
                     persistence=False, uncertain_concurrency=False)
        w.route('bugfix/light-codex', facts)
        for key in facts:
            changed = dict(facts); changed[key] = not facts[key]
            with self.assertRaises(ValueError):
                w.route('bugfix/light-codex', changed)
        facts['unknown'] = False
        with self.assertRaises(ValueError):
            w.route('bugfix/light-codex', facts)

    def test_migration_inventory_preserves_pins_and_blocks_active_tasks(self):
        tasks = [{'task_id': 'active', 'phase': {'kind': 'planned'}},
                 {'task_id': 'historical', 'phase': {'kind': 'finished'}}]
        old = {'packages': {'default/implementation': {}, 'kernel/report': {}}}
        real = w.toml
        with mock.patch.object(w, 'inventory', return_value=tasks), mock.patch.object(w, 'toml', side_effect=lambda p: old if Path(p).name == 'task-catalog.toml' else real(p)):
            value = w.migration('af', Path('/project'), Path('/private/state'))
        self.assertFalse(value['ready'])
        self.assertEqual(value['active_tasks'], ['active'])
        self.assertEqual(value['preserve_tasks'], ['active', 'historical'])
        self.assertEqual(value['remove_packages'], ['default/implementation', 'kernel/report'])

    def test_composition_requires_exact_goal_requirements_acceptance_and_issue(self):
        for field, value in [('goal', 'another issue'), ('requirements', {'acceptance': ['weaker']}),
                             ('acceptance', [{'id': 'a', 'text': 'different'}]),
                             ('issue', {'url': 'https://github.com/o/r/issues/2'})]:
            pins = [pin('bugfix/default-codex'), pin(w.catalog()['verification'], 'verification')]
            pins[-1]['template']['document'][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                w.composition('bugfix/default-codex', pins, FACTS)

    def test_github_identity_requires_canonical_url_paths(self):
        self.assertEqual(w.github_repository('https://github.com/Owner/Repo/issues/1', 'issues'),
                         ('owner', 'repo'))
        for value in (None, 1, 'https://github.com/o/r/issues/0',
                      'https://github.com/o/r/issues/1#other',
                      'https://github.com/o/r/issues/1?x=1',
                      'https://github.com/o/r/pull/1', 'https://example.com/o/r/issues/1',
                      'https://github.com/o/../issues/1', 'https://github.com/o/r?q/issues/1'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                w.github_repository(value, 'issues')

    def test_reconciliation_native_provenance_and_cumulative_accounting(self):
        # The native integration test separately proves these metadata paths against real CAS.
        for name, choice in w.catalog()['selections'].items():
            if name == 'design/astra-kimi':
                continue  # Design-only acceptance/provenance is covered separately.
            with self.subTest(selection=name):
                manifest, values, artifacts = pilot(name)
                def reconcile(m=manifest):
                    with mock.patch.object(w, 'invoke', side_effect=lambda *a: values[a[-1]]), \
                         mock.patch.object(w, 'recorded', side_effect=lambda state, identity, kind=None:
                                           artifacts[identity]['payload'] if kind else artifacts[identity]), \
                         mock.patch.object(w, 'validate_plan', side_effect=lambda n, v, state:
                                           check_selection(n, v)), \
                         mock.patch.object(w.subprocess, 'run', return_value=mock.Mock(stdout=b'root\n')):
                        return w.reconcile('af', '/project', '/state', m)
                result = reconcile()
                self.assertEqual(result['chain_chargeable_tokens'], '40' if choice['research_pipeline'] else '30')
                self.assertEqual(result['unknown_usage_attempts'], 2 if choice['research_pipeline'] else 1)
                self.assertFalse(result['quality_or_savings_claim'])
                self.assertEqual(result['schema'], 'af.issue-workflow-reconciliation/2')
                self.assertEqual(result['pull_request_association'], 'absent')
                self.assertFalse(result['remote_lookup_performed'])
                linked = dict(manifest, pull_request='https://github.com/O/R/pull/2')
                linked_result = reconcile(linked)
                self.assertEqual(linked_result['pull_request_association'], 'unverified')
                self.assertFalse(linked_result['remote_lookup_performed'])
                for issue_url in ('https://github.com/o/r/issues/2', 'https://github.com/o/other/issues/1'):
                    with self.assertRaisesRegex(ValueError, 'original native requirements.issue_url'):
                        reconcile(dict(manifest, issue=issue_url))
                for pr in ('https://github.com/o/other/pull/2', 'https://github.com/other/r/pull/2'):
                    with self.assertRaisesRegex(ValueError, 'PR repository differs'):
                        reconcile(dict(manifest, pull_request=pr))
                original_requirements = artifacts['original-requirements']['payload']['specification']
                saved_url = original_requirements.pop('issue_url')
                with self.assertRaisesRegex(ValueError, 'original native requirements.issue_url'):
                    reconcile()
                original_requirements['issue_url'] = saved_url
                self.assertEqual(len(result['tasks']), len(values))
                # Independent verification may accept a candidate its author did not verify;
                # native source binding carries that exact unsatisfied result, not acceptance.
                values['repair']['result']['acceptance'] = 'unsatisfied'
                ref = values['verify']['input_bindings']['bindings']['source']['task']
                ref['acceptance'] = 'unsatisfied'
                self.assertEqual(reconcile()['chain_chargeable_tokens'], result['chain_chargeable_tokens'])
                values['repair']['result']['acceptance'] = ref['acceptance'] = 'satisfied'
                for edit in [lambda m: m.update(source_commit='2' * 40),
                             lambda m: m.update(selection='impl/default-codex' if name != 'impl/default-codex' else 'bugfix/default-codex'),
                             lambda m: m.update(tasks=['repair']),
                             lambda m: m.update(tasks=['original', 'original']),
                             lambda m: m.update(research_tasks=['unrelated']),
                             lambda m: m.update(research_tasks=['research', 'research'])]:
                    bad = copy.deepcopy(manifest); edit(bad)
                    with self.assertRaises((ValueError, KeyError)):
                        reconcile(bad)
                if choice['research_pipeline']:
                    # A fully accepted, same-Pipeline/same-source Task still cannot replace
                    # the actual producer named by the implementation's native binding.
                    values['unrelated'] = copy.deepcopy(values['research'])
                    values['unrelated'].update(task_id='unrelated', revision_id='unrelated-revision')
                    artifacts['unrelated-revision'] = copy.deepcopy(artifacts['research-revision'])
                    artifacts['unrelated-revision']['payload']['task_id'] = 'unrelated'
                    bad = copy.deepcopy(manifest); bad['research_tasks'] = ['unrelated']
                    with self.assertRaisesRegex(ValueError, 'omits required research producer'):
                        reconcile(bad)
                    del values['unrelated'], artifacts['unrelated-revision']
                    # These are accepted, same-source, same-Pipeline native-shaped records.
                    # Change only their recorded business contract, retaining every binding.
                    target = artifacts['research-requirements']['payload']
                    saved = copy.deepcopy(target['specification'])
                    for wrong in ({'acceptance': ['stage only']},
                                  research_requirements('unrelated issue'),
                                  research_requirements('issue goal', {'acceptance': ['unrelated']}),
                                  research_requirements('issue goal', {'acceptance': ['exact'],
                                                        'issue_url': 'https://github.com/o/r/issues/2'})):
                        target['specification'] = wrong
                        with self.assertRaisesRegex(ValueError, 'original issue contract'):
                            reconcile()
                    target['specification'] = saved
                    # Optional native TaskAcceptance items are also part of the business
                    # contract; the Report stage still has its own completion acceptance.
                    items = [{'id': 'issue', 'text': 'exact original behavior'}]
                    artifacts['acceptance'] = {'type': 'af/TaskAcceptance@1', 'payload':
                                               {'schema': 'af.task-acceptance/1', 'items': items}}
                    for task in manifest['tasks'] + ['verify']:
                        artifacts[task + '-revision']['payload']['provenance']['input_artifact_ids'].append('acceptance')
                    with self.assertRaisesRegex(ValueError, 'original issue contract'):
                        reconcile()
                    target['specification']['original_issue']['acceptance'] = items
                    self.assertEqual(reconcile()['chain_chargeable_tokens'], result['chain_chargeable_tokens'])
                    target['specification']['original_issue']['acceptance'] = None
                    for task in manifest['tasks'] + ['verify']:
                        artifacts[task + '-revision']['payload']['provenance']['input_artifact_ids'].remove('acceptance')
                    inputs = values['research']['plan']['inputs']
                    saved_source = inputs['source']; inputs['source'] = {'snapshot_id': 'source'}
                    with self.assertRaises(ValueError):
                        reconcile()
                    inputs['source'] = saved_source
                mutations = [('verify-requirements', 'payload', 'specification', {'acceptance': ['weaker']}),
                             ('verify-revision', 'payload', 'goal', 'another goal'),
                             ('origin', None, 'repository_id', 'wrong-repo')]
                if choice['research_pipeline']:
                    bad = copy.deepcopy(manifest); bad['research_tasks'] = []
                    with self.assertRaises(ValueError):
                        reconcile(bad)

                for identity, nested, field, changed in mutations:
                    target = artifacts[identity][nested] if nested else artifacts[identity]
                    old = target[field]; target[field] = changed
                    with self.assertRaises(ValueError):
                        reconcile()
                    target[field] = old
                # Fail closed on unavailable checked metadata.
                saved = artifacts.pop('origin')
                with self.assertRaises(KeyError):
                    reconcile()
                artifacts['origin'] = saved
                # Wrong selected Pipeline, output revision, result, artifact or source binding.
                targets = [(values['verify']['selection']['assessment']['decision'], 'pipeline', 'other'),
                           (values['verify']['input_bindings']['bindings']['source']['task'], 'result_id', 'wrong'),
                           (values['verify']['input_bindings']['bindings']['source'], 'snapshot_id', 'wrong'),
                           (artifacts['repair-origin']['bound_from']['task'], 'task_revision_id', 'wrong')]
                if choice['research_pipeline']:
                    ref = values['original']['input_bindings']['bindings']['research_report']['task']
                    targets += [(ref, 'task_revision_id', 'wrong'), (ref, 'result_id', 'wrong'),
                                (ref, 'port', 'snapshot'),
                                (values['original']['input_bindings']['bindings']['research_report'], 'artifact_id', 'wrong-report'),
                                (values['research']['selection']['assessment']['decision'], 'pipeline', 'other')]
                for target, field, changed in targets:
                    old = target[field]; target[field] = changed
                    with self.assertRaises(ValueError):
                        reconcile()
                    target[field] = old

    def test_compose_refuses_nonpersonal_or_slot_replacement_bindings(self):
        p = pin('bugfix/default-codex')
        p['template']['bindings'] = {'schema': 'af.task-bindings/1', 'providers': {'x': 'codex-work'}}
        with self.assertRaises(ValueError):
            w.composition('bugfix/default-codex', [p, pin(w.catalog()['verification'], 'verification')], FACTS)
        p['template']['bindings'] = {'schema': 'af.task-bindings/1', 'slots': {}}
        with self.assertRaises(ValueError):
            w.composition('bugfix/default-codex', [p, pin(w.catalog()['verification'], 'verification')], FACTS)


if __name__ == '__main__':
    unittest.main()
