#!/usr/bin/env python3
"""Hermetic issue Workflow routing, composition, history and plan refusal tests."""
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
                                      'goal': 'Bounded issue fixture', 'base': {'commit': '1' * 40}, 'pipeline': {'name': pipeline, 'fallback': 'refuse'},
                                      'limits': {'tokens': 1000, 'wall_ms': 1000}}}}


FACTS = w.read(ROOT / 'fixtures/issue-workflows/routing-known.json')


class Workflows(unittest.TestCase):
    def setUp(self):
        patcher = mock.patch.object(w, 'captured_catalog')
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_all_seven_are_composed_from_native_tasks_with_no_actions(self):
        for name, selection in w.catalog()['selections'].items():
            research = selection['research_pipeline']
            pins = ([pin(research, 'report')] if research else []) + [pin(name), pin(w.catalog()['verification'], 'verification')]
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

    def test_reconcile_sums_each_task_once_retains_unknown_and_refuses_incomplete_chain(self):
        manifest = {'schema': 'af.issue-workflow-pilot/1', 'issue': 'https://github.com/o/r/issues/1',
                    'pull_request': None, 'source_commit': '1' * 40,
                    'selection': 'bugfix/light-codex', 'tasks': ['original', 'repair']}
        def inspection(task):
            return {'schema': 'af/task-inspection@20', 'task_id': task, 'revision_id': 'revision-' + task,
                    'plan_id': 'plan-' + task, 'chargeable_tokens': '10', 'state': {},
                    **({'continues': {'task_id': 'original', 'snapshot_id': 'snapshot'}, 'unknown_usage_attempts': 1} if task == 'repair' else {})}
        with mock.patch.object(w, 'invoke', side_effect=lambda *a: inspection(a[-1])):
            value = w.reconcile('af', '/project', '/state', manifest)
            self.assertEqual(value['chain_chargeable_tokens'], '20')
            self.assertEqual(value['unknown_usage_attempts'], 1)
            manifest['tasks'] = ['repair']
            with self.assertRaises(ValueError):
                w.reconcile('af', '/project', '/state', manifest)
        manifest['tasks'] = ['original', 'original']
        with self.assertRaises(ValueError):
            w.reconcile('af', '/project', '/state', manifest)

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
