#!/usr/bin/env python3
"""Hermetic issue Workflow routing, composition, history and plan refusal tests."""
import copy
import importlib.util
import json
from pathlib import Path
import sys
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
        artifacts[task + '-requirements'] = {'type': 'af/Requirements@1', 'payload': {'specification': {'acceptance': ['exact']}}}
        if task == 'research':
            artifacts[task + '-requirements']['payload']['specification'] = research_requirements('issue goal')
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

FACTS = w.read(ROOT / 'fixtures/issue-workflows/routing-known.json')


class Workflows(unittest.TestCase):
    def setUp(self):
        patcher = mock.patch.object(w, 'captured_catalog')
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_installed_and_captured_authority_pin_both_real_policies(self):
        staged = w.STAGED
        for kind in ('code', 'report'):
            self.assertEqual((staged / ('approved-' + kind + '-policy.toml')).read_bytes(),
                             (ROOT / ('.af/' + kind + '-policy.toml')).read_bytes())
        files = {'task-catalog.toml': 'replacement-catalog.toml',
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

    def test_effective_policy_identity_review_binding_and_local_execution(self):
        revision = {'authority': {'policy_id': 'authority'}}
        for kind in ('code', 'report'):
            authority = {kind + '_policy_id': w.approved_policy_id(kind)}
            value = {'revision_id': 'revision', 'plan': {'authority': {'policy_id': 'authority'}},
                     'selection': {'assessment': {'decision': {'pipeline':
                         'issue-internal/report' if kind == 'report' else 'bugfix/default-codex'}}},
                     'graph': {'nodes': {}}}
            def recorded(state, identity, artifact_type=None):
                return revision if identity == 'revision' else authority
            with mock.patch.object(w, 'recorded', side_effect=recorded):
                w.validate_policy(value, '/state')
                authority[kind + '_policy_id'] = 'wrong'
                with self.assertRaisesRegex(ValueError, 'policy identity'):
                    w.validate_policy(value, '/state')
                authority[kind + '_policy_id'] = w.approved_policy_id(kind)
                value['graph']['nodes']['gate'] = {'operator': {'operator': {'remote_checks': ['kernel']}}}
                with self.assertRaisesRegex(ValueError, 'locally'):
                    w.validate_policy(value, '/state')
                value['graph']['nodes'] = {}
                value['plan']['authority']['policy_id'] = 'forged'
                with self.assertRaisesRegex(ValueError, 'native revision'):
                    w.validate_policy(value, '/state')
                value['plan']['authority']['policy_id'] = 'authority'
        authority = {'code_policy_id': w.approved_policy_id('code'), 'review_policy_id': 'review'}
        value['selection']['assessment']['decision']['pipeline'] = 'bugfix/default-codex'
        with mock.patch.object(w, 'recorded', side_effect=[revision, authority, {}, {'check_policy_id': 'wrong'}]):
            with self.assertRaisesRegex(ValueError, 'Review check policy'):
                w.validate_policy(value, '/state')

    def test_all_seven_are_composed_from_native_tasks_with_no_actions(self):
        for name, selection in w.catalog()['selections'].items():
            research = selection['research_pipeline']
            pins = ([pin(research, 'report')] if research else []) + [pin(name), pin(w.catalog()['verification'], 'verification')]
            if research:
                pins[0]['template']['document']['goal'] = 'Distinct research question'
                pins[0]['template']['document']['requirements'] = research_requirements()
            composed = w.composition(name, pins, FACTS)['definitions']['issue-workflow']
            self.assertEqual(composed['authority']['grants'], [])
            self.assertEqual(composed['authority']['decision_owners'], [])
            steps = {s['id']: s for s in composed['steps']}
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
        for name in ('bugfix/research-fix-codex', 'impl/design-codex'):
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
        self.assertEqual(files, ['design.json', 'diagnose.json', 'known-fix.json',
                                 'research-fix.json', 'verify.json'])

    def test_only_ordinary_default_drops_the_standard_selector_fact(self):
        staged = ROOT / 'fixtures/issue-workflows'
        contracts = w.read(staged / 'contracts.json')['pipelines']
        for name in w.catalog()['selections']:
            pipeline = w.toml(staged / 'task-packages' / name / 'pipeline.toml')
            facts = pipeline['accepts']['required_facts']
            self.assertEqual(facts, contracts[name]['accepts']['required_facts'])
            if name == 'bugfix/default-codex':
                self.assertEqual(facts, {})
            elif '/light-' not in name:
                self.assertIs(facts['standard'], True)

    def test_old_public_names_are_refused_without_aliases(self):
        for family in ('default', 'default-light', 'pi-light', 'warm', 'kernel', 'owner-default'):
            with self.assertRaises(ValueError):
                w.selection(family + '/implementation')
        self.assertEqual(len(w.catalog()['selections']), 7)

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

    def test_reconciliation_native_provenance_and_cumulative_accounting(self):
        # The native integration test separately proves these metadata paths against real CAS.
        for name, choice in w.catalog()['selections'].items():
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
                                  research_requirements('issue goal', {'acceptance': ['unrelated']})):
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
