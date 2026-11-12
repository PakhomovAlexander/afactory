#!/usr/bin/env python3
"""Hermetic Phase 1 contracts, offline/production observation, recurrence and remedy acceptance."""
import copy
import importlib.util
import itertools
import io
import inspect
import json
import runpy
from contextlib import closing, redirect_stdout
from unittest.mock import patch
import urllib.error
import urllib.request
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location('ci_health', ROOT / 'scripts/ci-health.py')
C = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(C)
FIXTURE = ROOT / 'fixtures/scenarios/maintenance-ci-health'


def service():
    return json.loads((FIXTURE / 'failing-service.json').read_text())


def release_service(main_failure=True):
    github = json.loads((FIXTURE / 'release-github-service.json').read_text())
    if not main_failure:
        github['routes']['repos/octo/project/actions/runs/101/attempts/1/jobs?per_page=100']['jobs'][0]['conclusion'] = 'success'
    raw = service()
    collected = C.collect(C.OfflineGithub(github['repository'], github['routes'], github['max_requests']),
        captured_at=raw['captured_at'], main_ref=github['main_ref'], since=github['since'],
        nightly_receipt=raw['nightly_receipt'], release_pr=7)
    return collected, github


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.journal = C.Journal(Path(self.temp.name) / 'observer.sqlite')
        self.addCleanup(self.temp.cleanup)
        self.addCleanup(self.journal.close)

    def observe(self, value=None):
        return C.observe(value or service(), self.journal)

    def diagnosis(self, snapshot, **changes):
        result = C.malformed(C.digest(snapshot), snapshot, snapshot['failures'][0], 'fixture')
        result.update(category='product_bug', rationale='Exact assertion evidence', **changes)
        return result

    def test_ci_observe_v1_uses_bounded_gets_for_exact_heads_and_run_attempts(self):
        fixture = json.loads((FIXTURE / 'github-service.json').read_text())
        raw = service()
        client = C.OfflineGithub(fixture['repository'], fixture['routes'], fixture['max_requests'])
        collected = C.collect(client, captured_at=raw['captured_at'], main_ref=fixture['main_ref'],
                              since=fixture['since'], nightly_receipt=raw['nightly_receipt'])
        self.assertEqual(collected, raw)
        self.assertEqual(len(client.requests), 3)
        routes = copy.deepcopy(fixture['routes'])
        routes['repos/octo/project/pulls/7'] = {'number': 7, 'state': 'open', 'head': {'sha': raw['source_sha']}}
        release = C.collect(C.OfflineGithub('octo/project', routes), captured_at=raw['captured_at'],
            main_ref='main', since=fixture['since'], nightly_receipt=raw['nightly_receipt'], release_pr=7)
        self.assertEqual(release['release_pr'], {'number': 7, 'head_sha': raw['source_sha']})
        self.assertTrue(all(job['release_blocking'] for job in release['jobs']))
        self.assertTrue(all(method == 'GET' for method, _ in client.requests))
        limited = C.OfflineGithub(fixture['repository'], fixture['routes'], 2)
        incomplete = self.observe(C.collect(limited, captured_at=raw['captured_at'], main_ref='main',
            since=fixture['since'], nightly_receipt=raw['nightly_receipt']))
        self.assertFalse(incomplete['complete'])
        self.assertIn('GitHub request bound exhausted', incomplete['omissions'])
        self.assertEqual(len(limited.requests), 2)
        with self.assertRaisesRegex(ValueError, 'escaped repository'):
            client.get('repos/another/project/actions/runs')
        with self.assertRaisesRegex(ValueError, 'Invalid main ref'):
            C.collect(client, captured_at=raw['captured_at'], main_ref='../secret',
                since=fixture['since'], nightly_receipt=raw['nightly_receipt'])
        for key in ('head_sha', 'run_attempt'):
            routes = copy.deepcopy(fixture['routes'])
            route = next(route for route in routes if 'head_sha=' in route)
            del routes[route]['workflow_runs'][0][key]
            incomplete = self.observe(C.collect(C.OfflineGithub('octo/project', routes),
                captured_at=raw['captured_at'], main_ref='main', since=fixture['since'],
                nightly_receipt=raw['nightly_receipt']))
            self.assertFalse(incomplete['complete'])
            self.assertEqual(incomplete['failures'], [])

    def test_duplicate_and_nonfinite_json_evidence_is_refused(self):
        for raw in ['{"schema":"wrong","schema":"af.ci-diagnosis/1"}', '{"value":NaN}', '{"value":Infinity}']:
            with self.assertRaises(ValueError):
                C.loads(raw)

    def test_maximum_complete_snapshot_has_100_failures_and_exact_64_kib_serialized_bound(self):
        raw = (FIXTURE / 'max-snapshot.json').read_bytes()
        self.assertEqual(len(raw), 65536)  # The native CLI also bounds the input file itself.
        snapshot = json.loads(raw)
        C.validate_snapshot(snapshot)
        self.assertTrue(snapshot['complete'])
        self.assertEqual(snapshot['omissions'], [])
        self.assertEqual(len(snapshot['failures']), 100)
        self.assertEqual(len(C.canonical(snapshot)), 65536)
        # Test the payload's serialized boundary, including escaping and UTF-8, rather
        # than file whitespace. Even one extra admitted string byte must be refused.
        self.assertIn('核"\\', snapshot['failures'][0]['normalized_fingerprint'])
        snapshot['repository'] += 'x'
        self.assertEqual(len(C.canonical(snapshot)), 65537)
        with self.assertRaisesRegex(ValueError, 'CI evidence exceeds byte bound'):
            C.validate_snapshot(snapshot)

    def test_distinct_release_head_binds_deterministic_and_model_diagnoses_and_refuses_main(self):
        raw, _ = release_service(main_failure=False)
        snapshot = self.observe(raw)
        failure, = snapshot['failures']
        self.assertTrue(failure['release_blocking'])
        self.assertNotEqual(snapshot['source_sha'], failure['head_sha'])
        diagnosis = self.diagnosis(snapshot)
        self.assertEqual(diagnosis['source_sha'], failure['head_sha'])
        self.assertEqual(diagnosis['evidence'][0]['source_sha'], failure['head_sha'])
        C.validate_diagnosis(diagnosis, C.digest(snapshot), snapshot)
        for flags in ({}, {'known_transients': [failure['signature_id']]}, {'infrastructure': [failure['signature_id']]}):
            result, _ = C.diagnose(C.digest(snapshot), snapshot, failure, diagnosis, **flags)
            C.validate_diagnosis(result, C.digest(snapshot), snapshot)
            self.assertEqual(result['source_sha'], failure['head_sha'])
        for field in ('diagnosis', 'evidence', 'both'):
            wrong = copy.deepcopy(diagnosis)
            if field != 'evidence':
                wrong['source_sha'] = snapshot['source_sha']
            if field != 'diagnosis':
                wrong['evidence'][0]['source_sha'] = snapshot['source_sha']
            with self.subTest(field=field), self.assertRaises(ValueError):
                C.validate_diagnosis(wrong, C.digest(snapshot), snapshot)
            result, invoked = C.diagnose(C.digest(snapshot), snapshot, failure, wrong)
            self.assertTrue(invoked)
            self.assertEqual(result['category'], 'unknown')
            C.validate_diagnosis(result, C.digest(snapshot), snapshot)

    def test_scenario_compiler_bypasses_diagnosis_for_exact_signatures_and_reports_bounds(self):
        spec = importlib.util.spec_from_file_location('ci_scenario', FIXTURE / 'configure.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        snapshot = self.observe()
        target = {'repo': '/fixture/repo', 'state': '/fixture/state', 'task_id': 'diagnose',
                  'plan_id': 'sha256:' + 'd' * 64, 'revision_id': 'sha256:' + 'e' * 64, 'max_tokens': 24096}
        signature = snapshot['failures'][0]['signature_id']
        for flags, row in [({'known_transients': [signature]}, 4), ({'infrastructure': [signature]}, 3), ({'exhausted': True}, 1)]:
            rendered = module.bundle(snapshot, C.digest(snapshot), target, **flags)
            steps = rendered['definitions']['maintenance-ci-health']['steps']
            self.assertFalse(any(s['operation']['kind'] in ('task', 'ci_diagnosis') for s in steps))
            self.assertEqual([s['id'] for s in steps], ['observe', 'notify-findings', 'remedy-' + str(row), 'report-outcome', 'watch'])
            message = json.loads(steps[1]['inputs']['body']['value']['value'])
            C.structural(message, 'ci-health-notification')
            C.validate_diagnosis(message['deterministic_results'][0]['diagnosis'], C.digest(snapshot), snapshot)
        release = copy.deepcopy(snapshot)
        release['failures'][0]['release_blocking'] = True
        steps = module.bundle(release, C.digest(release), target, known_transients=[signature])['definitions']['maintenance-ci-health']['steps']
        self.assertEqual(steps[2]['id'], 'remedy-2')

    def test_bundle_distinct_release_head_pinned_classifiers_select_row_two_release_alert(self):
        spec = importlib.util.spec_from_file_location('ci_scenario', FIXTURE / 'configure.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        raw, _ = release_service(main_failure=False)
        snapshot = self.observe(raw)
        failure, = snapshot['failures']
        self.assertNotEqual(snapshot['source_sha'], failure['head_sha'])
        target = {'repo': '/fixture/repo', 'state': '/fixture/state', 'task_id': 'diagnose',
                  'plan_id': C.digest('plan'), 'revision_id': C.digest('revision'), 'max_tokens': 24096}
        for classifier, category in [('known_transients', 'transient'), ('infrastructure', 'ci_infra')]:
            with self.subTest(classifier=classifier):
                rendered = module.bundle(snapshot, C.digest(snapshot), target,
                    **{classifier: [failure['signature_id']]})
                steps = rendered['definitions'][rendered['root']]['steps']
                self.assertFalse(any(s['operation']['kind'] == 'ci_diagnosis' for s in steps))
                remedies = [s for s in steps if s['id'].startswith('remedy-')]
                self.assertEqual([s['id'] for s in remedies], ['remedy-2'])
                message = C.loads(remedies[0]['inputs']['body']['value']['value'])
                C.structural(message, 'ci-health-notification')
                self.assertEqual((message['row'], message['intended']), (2, 'release_alert'))
                diagnosis = message['deterministic_diagnosis']
                C.validate_diagnosis(diagnosis, C.digest(snapshot), snapshot)
                self.assertEqual((diagnosis['source_sha'], diagnosis['category']), (failure['head_sha'], category))
                wrong = {**diagnosis, 'source_sha': snapshot['source_sha']}
                self.assertEqual(C.remedy(snapshot, failure, wrong), (1, 'alert_only'))

    def test_all_occurrences_are_classified_before_one_unresolved_diagnosis_is_selected(self):
        spec = importlib.util.spec_from_file_location('ci_scenario', FIXTURE / 'configure.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        raw = service()
        raw['jobs'] = [dict(raw['jobs'][0], run_id=i + 1, job_id=i + 1,
            job_name='job-' + str(i)) for i in range(4)]
        snapshot = self.observe(raw)
        first, second, third, fourth = snapshot['failures']
        target = {'repo': '/fixture/repo', 'state': '/fixture/state', 'task_id': 'diagnose',
                  'plan_id': C.digest('plan'), 'revision_id': C.digest('revision'), 'max_tokens': 24096}
        for classifier in ('known_transients', 'infrastructure'):
            with self.subTest(classifier=classifier):
                flags = {classifier: [first['signature_id'], third['signature_id']]}
                steps = module.bundle(snapshot, C.digest(snapshot), target, **flags)['definitions']['maintenance-ci-health']['steps']
                self.assertEqual(sum(s['operation']['kind'] == 'ci_diagnosis' for s in steps), 1)
                message = C.loads(steps[1]['inputs']['body']['value']['value'])
                C.structural(message, 'ci-health-notification')
                self.assertEqual(message['occurrence_id'], second['occurrence_id'])
                self.assertEqual(message['unselected_occurrences'], [fourth['occurrence_id']])
                self.assertEqual(message['unselected_reason'], 'diagnosis_reservation_exhausted')
                results = message['deterministic_results']
                self.assertEqual([r['diagnosis']['occurrence_id'] for r in results],
                                 [f['occurrence_id'] for f in (first, third, fourth)])
                category, row, intended = ('transient', 4, 'rerun_intended') if classifier == 'known_transients' else ('ci_infra', 3, 'infrastructure_alert')
                for result, failure in zip(results, (first, third, fourth)):
                    C.validate_diagnosis(result['diagnosis'], C.digest(snapshot), snapshot)
                    if failure == fourth:
                        self.assertEqual(result['diagnosis']['category'], 'unknown')
                        self.assertEqual((result['row'], result['intended']), (1, 'alert_only'))
                    else:
                        self.assertEqual(result['diagnosis']['category'], category)
                        self.assertEqual((result['row'], result['intended']), (row, intended))
                flags[classifier] += [second['signature_id'], fourth['signature_id']]
                steps = module.bundle(snapshot, C.digest(snapshot), target, **flags)['definitions']['maintenance-ci-health']['steps']
                self.assertFalse(any(s['operation']['kind'] == 'ci_diagnosis' for s in steps))
                message = C.loads(steps[1]['inputs']['body']['value']['value'])
                self.assertEqual(message['unselected_occurrences'], [])
                self.assertIsNone(message['unselected_reason'])
                self.assertEqual([r['diagnosis']['occurrence_id'] for r in message['deterministic_results']],
                                 [f['occurrence_id'] for f in snapshot['failures']])
                self.assertTrue(all(r['diagnosis']['category'] == category and (r['row'], r['intended']) == (row, intended)
                                    for r in message['deterministic_results']))

    def test_maximum_snapshot_reports_every_deterministic_diagnosis_and_remedy_within_request_and_dispatch_bounds(self):
        spec = importlib.util.spec_from_file_location('ci_scenario', FIXTURE / 'configure.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        snapshot = json.loads((FIXTURE / 'max-snapshot.json').read_text())
        target = {'repo': '/fixture/repo', 'state': '/fixture/state', 'task_id': 'diagnose',
                  'plan_id': C.digest('plan'), 'revision_id': C.digest('revision'), 'max_tokens': 24096}
        for flags, count, category, remedy in [
                ({'infrastructure': [f['signature_id'] for f in snapshot['failures']]}, 100, 'ci_infra', (3, 'infrastructure_alert')),
                ({}, 99, 'unknown', (1, 'alert_only')),
                ({'exhausted': True}, 100, 'unknown', (1, 'alert_only'))]:
            with self.subTest(flags=list(flags)):
                bundle = module.bundle(snapshot, C.digest(snapshot), target, **flags)
                definition = bundle['definitions'][bundle['root']]
                self.assertEqual(definition['limits']['max_dispatches'], 20)
                self.assertEqual(definition['limits']['max_tokens'], 24096)
                self.assertLessEqual(len(definition['steps']), 20)
                self.assertLessEqual(len(C.canonical(bundle)), 16 * 1024 * 1024)
                findings = [s for s in definition['steps'] if s['id'].startswith('notify-findings')]
                messages = [C.loads(s['inputs']['body']['value']['value']) for s in findings]
                self.assertEqual([f for m in messages for f in m['failures']], C.failure_summaries(snapshot))
                results = [r for m in messages for r in m['deterministic_results']]
                self.assertEqual(len(results), count)
                self.assertEqual([r['diagnosis']['occurrence_id'] for r in results],
                                 [f['occurrence_id'] for f in snapshot['failures'][100 - count:]])
                for result in results:
                    C.validate_diagnosis(result['diagnosis'], C.digest(snapshot), snapshot)
                    self.assertEqual(result['diagnosis']['category'], category)
                    self.assertEqual((result['row'], result['intended']), remedy)
                for step, message in zip(findings, messages):
                    C.structural(message, 'ci-health-notification')
                    inputs = {key: value['value'] for key, value in step['inputs'].items() if value['source'] == 'literal'}
                    inputs['subject'] = {'type': 'text', 'value': C.digest(snapshot['repository'])}
                    request = {'schema': 'af/scenario-adapter-request@4', 'operation': 'notify', 'method': 'reconcile',
                        'intent_id': C.digest(step['id']), 'input_digest': C.digest(inputs),
                        'destination': 'fixture:ci-health', 'inputs': inputs}
                    self.assertLessEqual(len(C.canonical(request)), 65536)

    def test_large_valid_scopes_complete_every_notification_phase_within_request_and_dispatch_bounds(self):
        spec = importlib.util.spec_from_file_location('ci_scenario', FIXTURE / 'configure.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        target = {'repo': '/fixture/repo', 'state': '/fixture/state', 'task_id': 'diagnose',
                  'plan_id': C.digest('plan'), 'revision_id': C.digest('revision'), 'max_tokens': 24096}
        for count, scope_size in [(1, 40000), (1, 64000), (100, 4)]:
            with self.subTest(count=count), tempfile.TemporaryDirectory() as directory:
                snapshot = json.loads((FIXTURE / ('max-snapshot.json' if count > 1 else 'snapshot.json')).read_text())
                snapshot['repository'] = 'r' * scope_size + '/project'
                snapshot['failures'] = snapshot['failures'][:count]
                for failure in snapshot['failures']:
                    failure['occurrence_id'] = C.occurrence_id(snapshot['repository'], failure)
                    failure['signature_id'] = C.signature_id(snapshot['repository'], failure,
                        failure['occurrence_id'], failure['normalized_fingerprint'])
                C.validate_snapshot(snapshot)
                rendered = module.bundle(snapshot, C.digest(snapshot), target,
                    infrastructure=[f['signature_id'] for f in snapshot['failures']])
                definition = rendered['definitions'][rendered['root']]
                findings = [s for s in definition['steps'] if s['id'].startswith('notify-findings')]
                self.assertEqual(definition['limits']['max_dispatches'], 20)
                self.assertLessEqual(len(definition['steps']), 20)
                if count > 1:
                    self.assertGreater(len(findings), 1)
                root = Path(directory)
                executable = root / 'adapter'
                executable.write_bytes(module.adapter_bytes(ROOT))
                executable.chmod(0o700)
                (root / 'service.json').write_bytes(C.canonical({'snapshot': snapshot,
                    'snapshot_id': C.digest(snapshot), 'scenario_run': 'ci-health-fixture'}))
                notifications = [s for s in definition['steps'] if s['operation']['kind'] == 'action']
                self.assertEqual([s['id'] for s in notifications[-2:]], ['remedy-3', 'report-outcome'])
                for step in notifications:
                    inputs = {key: value['value'] for key, value in step['inputs'].items() if value['source'] == 'literal'}
                    inputs['subject'] = {'type': 'text', 'value': C.digest(snapshot['repository'])}
                    body = C.loads(inputs['body']['value'])
                    C.structural(body, 'ci-health-notification')
                    self.assertNotIn('repository', body)
                    if body['phase'] == 'findings':
                        self.assertNotIn('deterministic_diagnosis', body)
                    else:
                        C.validate_diagnosis(body['deterministic_diagnosis'], C.digest(snapshot), snapshot)
                    request = {'schema': 'af/scenario-adapter-request@4', 'operation': 'notify', 'method': 'execute',
                        'intent_id': C.digest(step['id']), 'input_digest': C.digest(inputs),
                        'destination': 'fixture:ci-health', 'inputs': inputs}
                    self.assertLessEqual(len(C.canonical(request)), 65536)
                    result = subprocess.run([str(executable)], input=C.canonical(request), capture_output=True, check=True)
                    self.assertEqual(C.loads(result.stdout)['outcome'], 'done')
                messages = [C.loads(line) for line in (root / 'events').read_text().splitlines()]
                self.assertTrue(all(m['repository'] == snapshot['repository'] for m in messages))
                self.assertEqual([m['phase'] for m in messages[-2:]], ['remedy-intent', 'outcome'])
                self.assertEqual((messages[-2]['row'], messages[-2]['intended']), (3, 'infrastructure_alert'))
                self.assertEqual([f for m in messages for f in m['failures']], C.failure_summaries(snapshot))
                results = [r for m in messages for r in m['deterministic_results']]
                self.assertEqual(len(results), count)
                self.assertEqual([r['diagnosis']['occurrence_id'] for r in results],
                                 [f['occurrence_id'] for f in snapshot['failures']])
                for result in results:
                    C.validate_diagnosis(result['diagnosis'], C.digest(snapshot), snapshot)
                    self.assertEqual(result['diagnosis']['category'], 'ci_infra')
                    self.assertEqual((result['row'], result['intended']), (3, 'infrastructure_alert'))

    def test_large_scope_native_notification_transport_retains_refusal_and_rejects_old_evidence(self):
        spec = importlib.util.spec_from_file_location('ci_scenario', FIXTURE / 'configure.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        raw = service()
        raw['repository'] = 'r' * 61000 + '/project'
        snapshot = self.observe(raw)
        C.validate_snapshot(snapshot)
        sid = C.digest(snapshot)
        selected = snapshot['failures'][0]
        target = {'repo': '/fixture/repo', 'state': '/fixture/state', 'task_id': 'diagnose',
                  'plan_id': C.digest('plan'), 'revision_id': C.digest('revision'), 'max_tokens': 24096}
        diagnosis = self.diagnosis(snapshot)
        diagnosis.update(category='unknown',
            rationale='Deterministic unknown: native diagnosis Task output was refused')
        selection = {'schema': 'af.ci-diagnosis-selection/1',
            **{key: diagnosis[key] for key in ('occurrence_id', 'signature_id', 'source_sha')}}
        record = {'schema': 'af/scenario-ci-diagnosis@2', 'snapshot_id': sid,
            'selection': selection, 'diagnosis': diagnosis,
            'receipt': {'task_id': target['task_id'], 'plan_id': target['plan_id'],
                'revision_id': target['revision_id'], 'result_id': C.digest('refused'),
                'charged_tokens': 5, 'result': {'task_revision_id': target['revision_id'],
                    'execution': 'exhausted', 'acceptance': 'inconclusive',
                    'domain_conclusion': 'incomplete', 'outputs': {}, 'evidence': [],
                    'missing_obligations': ['diagnosis']}},
            'provenance': {'kind': 'deterministic_refusal', 'refusals': [{
                'attempt_id': 'a' * 26, 'diagnostic_id': C.digest('diagnostic'),
                'error_prefix': 'CI diagnosis references evidence outside selected snapshot occurrence'}]}}
        record['provenance']['refusals'] = [
            {'attempt_id': str(i) * 26, 'diagnostic_id': C.digest('diagnostic-' + str(i)),
             'error_prefix': '\"\n\t' * 682 + 'xx'} for i in range(2)]
        self.assertTrue(all(len(r['error_prefix']) == 2048 for r in record['provenance']['refusals']))
        transport = {'schema': 'af/scenario-ci-notification-evidence@2',
            'repository_id': C.digest(snapshot['repository']), 'settlement_id': C.digest(record),
            **{k: record['receipt'][k] for k in ('task_id', 'plan_id', 'revision_id', 'result_id')},
            'snapshot_id': sid, 'selection': selection, 'category': 'unknown',
            'provenance': {'kind': 'deterministic_refusal', 'refusals': [
                {k: r[k] for k in ('attempt_id', 'diagnostic_id')}
                for r in record['provenance']['refusals']]},
            'omitted': 'details_retained_in_native_settlement'}
        C.structural(transport, 'scenario-ci-notification-evidence')
        definition = module.bundle(snapshot, sid, target)['definitions']['maintenance-ci-health']
        notifications = [s for s in definition['steps']
                         if s['id'] in ('notify-diagnosis', 'remedy-1', 'report-outcome')]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            executable = root / 'adapter'
            executable.write_bytes(module.adapter_bytes(ROOT))
            executable.chmod(0o700)
            (root / 'service.json').write_bytes(C.canonical({'snapshot': snapshot,
                'snapshot_id': sid, 'scenario_run': 'ci-health-fixture'}))
            for step in notifications:
                inputs = {key: value['value'] for key, value in step['inputs'].items()
                          if value['source'] == 'literal'}
                inputs.update(subject={'type': 'text', 'value': C.digest(snapshot['repository'])},
                    category={'type': 'text', 'value': 'unknown'},
                    signature_id={'type': 'text', 'value': selected['signature_id']},
                    diagnosis_settlement={'type': 'text', 'value': C.canonical(transport).decode()})
                request = {'schema': 'af/scenario-adapter-request@4', 'operation': 'notify', 'method': 'execute',
                    'intent_id': C.digest(step['id']), 'input_digest': C.digest(inputs),
                    'destination': 'fixture:ci-health', 'inputs': inputs}
                old = copy.deepcopy(request)
                old['inputs']['diagnosis_settlement'] = {'type': 'observation', 'value': {
                    'subject': snapshot['repository'], 'revision': selected['head_sha'],
                    'condition': 'ci_diagnosis_settled_v1', 'satisfied': True,
                    'evidence': C.canonical(record).decode()}}
                self.assertGreater(len(C.canonical(old)), 65536)
                full = copy.deepcopy(request)
                full['inputs']['subject']['value'] = snapshot['repository']
                full['inputs']['diagnosis_settlement']['value'] = C.canonical(
                    {'schema': 'af/scenario-ci-notification-evidence@1', 'settlement': record}).decode()
                self.assertGreater(len(C.canonical(full)), 65536)
                self.assertLessEqual(len(C.canonical(request)), 65536)
                result = subprocess.run([str(executable)], input=C.canonical(request), capture_output=True, check=True)
                self.assertEqual(C.loads(result.stdout)['outcome'], 'done')
            events = [C.loads(line) for line in (root / 'events').read_text().splitlines()]
            self.assertEqual([e['phase'] for e in events], ['diagnosis', 'remedy-intent', 'outcome'])
            for event in events:
                C.structural(event, 'ci-health-notification')
                self.assertEqual(event['repository'], snapshot['repository'])
                self.assertEqual(event['deterministic_diagnosis'], diagnosis)
                self.assertEqual(event['category'], 'unknown')
                self.assertEqual(event['native_evidence'], transport)
            self.assertEqual((events[1]['row'], events[1]['intended']), (1, 'alert_only'))
            for invalid in ('old-observation', 'old-version', 'missing-reference', 'bad-reference', 'wrong-repository', 'duplicate-refusal'):
                tampered = copy.deepcopy(request)
                evidence = copy.deepcopy(transport)
                if invalid == 'old-observation':
                    tampered = copy.deepcopy(old)
                    tampered['inputs']['diagnosis_settlement']['value']['subject'] = 'short/project'
                    self.assertLessEqual(len(C.canonical(tampered)), 65536)
                else:
                    if invalid == 'old-version':
                        evidence['schema'] = 'af/scenario-ci-notification-evidence@1'
                    elif invalid == 'missing-reference':
                        del evidence['settlement_id']
                    elif invalid == 'bad-reference':
                        evidence['result_id'] = 'not-a-digest'
                    elif invalid == 'wrong-repository':
                        evidence['repository_id'] = C.digest('foreign/repo')
                    else:
                        evidence['provenance']['refusals'][1] = evidence['provenance']['refusals'][0]
                    tampered['inputs']['diagnosis_settlement']['value'] = C.canonical(evidence).decode()
                with self.subTest(invalid=invalid):
                    refused = subprocess.run([str(executable)], input=C.canonical(tampered), capture_output=True)
                    self.assertNotEqual(refused.returncode, 0)
            self.assertEqual(len((root / 'events').read_text().splitlines()), 3)

    def test_contract_schema_parity_and_offline_cli_receipt(self):
        for name, value in C.schema_documents().items():
            self.assertEqual(json.loads((ROOT / 'schemas' / name).read_text()), value)
        result = subprocess.run([sys.executable, '-I', str(ROOT / 'scripts/ci-health.py'), 'observe',
            '--service', str(FIXTURE / 'failing-service.json'), '--state', str(Path(self.temp.name) / 'cli.sqlite')],
            capture_output=True, text=True, check=True)
        snapshot = C.validate_snapshot(json.loads(result.stdout))
        self.assertEqual(snapshot, self.observe())
        self.assertEqual(json.loads((FIXTURE / 'snapshot.json').read_text()), snapshot)

    def test_installed_observer_and_notifications_use_exact_evidence_and_durable_receipts(self):
        spec = importlib.util.spec_from_file_location('ci_scenario', FIXTURE / 'configure.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        for case in ('healthy', 'failing', 'incomplete', 'release', 'unselected-release'):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                raw = service()
                github = json.loads((FIXTURE / 'github-service.json').read_text())
                if case in ('release', 'unselected-release'):
                    raw, github = release_service(main_failure=case == 'unselected-release')
                if case == 'healthy':
                    raw['jobs'][0]['conclusion'] = 'success'
                    raw['nightly_receipt']['conclusion'] = 'success'
                    route = next(key for key in github['routes'] if '/jobs?' in key)
                    github['routes'][route]['jobs'][0]['conclusion'] = 'success'
                if case == 'incomplete':
                    raw['omissions'] = ['fixture unreadable logs']
                journal = C.Journal(root / 'expected.sqlite')
                try:
                    snapshot = C.observe(raw, journal)
                finally:
                    journal.close()
                if case == 'unselected-release':
                    self.assertEqual(snapshot, json.loads((FIXTURE / 'release-snapshot.json').read_text()))
                sid = C.digest(snapshot)
                (root / 'service.json').write_text(json.dumps({'snapshot': snapshot, 'snapshot_id': sid,
                    'observation_service': raw, 'github_service': github,
                    'scenario_run': 'ci-health-fixture', 'merged': False}))
                executable = root / 'adapter'
                executable.write_bytes(module.adapter_bytes(ROOT))
                executable.chmod(0o700)

                def invoke(operation, method, identity, **values):
                    inputs = {key: {'type': 'text', 'value': value} for key, value in {
                        'subject': C.digest(snapshot['repository']) if operation == 'notify' else snapshot['repository'],
                        'revision': snapshot['source_sha'], **values}.items()}
                    request = {'schema': 'af/scenario-adapter-request@4', 'operation': operation,
                        'method': method, 'intent_id': C.digest(identity), 'input_digest': C.digest(inputs),
                        'destination': 'fixture:ci-health', 'inputs': inputs}
                    result = subprocess.run([str(executable)], input=C.canonical(request), capture_output=True, check=True)
                    response = C.loads(result.stdout)
                    self.assertEqual(response['intent_id'], request['intent_id'])
                    self.assertEqual(response['input_digest'], request['input_digest'])
                    self.assertEqual(response['schema'], 'af/scenario-adapter-response@1')
                    return response

                observed = invoke('observe', 'observe', 'observe', condition='ci_observe_v1:' + C.digest(snapshot))
                self.assertEqual(observed['outcome'], 'done')
                self.assertTrue(observed['observation']['satisfied'])
                evidence = json.loads(observed['evidence'])
                self.assertEqual(evidence['complete'], case != 'incomplete')
                self.assertEqual(evidence['snapshot_id'], sid)
                target = {'repo': '/fixture/repo', 'state': '/fixture/state', 'task_id': 'diagnose',
                    'plan_id': C.digest('plan'), 'revision_id': C.digest('revision'), 'max_tokens': 24096}
                bundle = module.bundle(snapshot, sid, target, exhausted=case == 'unselected-release')
                step = next(s for s in bundle['definitions'][bundle['root']]['steps'] if s['id'] == 'notify-findings')
                body = step['inputs']['body']['value']['value']
                self.assertEqual(invoke('notify', 'reconcile', 'findings', body=body)['outcome'], 'absent')
                self.assertEqual(invoke('notify', 'execute', 'findings', body=body)['outcome'], 'done')
                self.assertEqual(invoke('notify', 'reconcile', 'findings', body=body)['outcome'], 'done')
                self.assertEqual(invoke('notify', 'execute', 'findings', body=body)['outcome'], 'done')
                events = [json.loads(line) for line in (root / 'events').read_text().splitlines()]
                self.assertEqual([event['phase'] for event in events], ['ci_observe_v1', 'findings'])
                self.assertTrue(events[1]['report_only'])
                self.assertEqual(events[1]['failures'], C.failure_summaries(snapshot))
                self.assertEqual(events[1]['release_alerts'], [f['occurrence_id'] for f in snapshot['failures'] if f['release_blocking']])
                if case == 'incomplete':
                    results = events[1]['deterministic_results']
                    self.assertEqual(len(results), 1)
                    self.assertEqual(results[0]['diagnosis']['category'], 'unknown')
                    self.assertEqual((results[0]['row'], results[0]['intended']), (1, 'alert_only'))
                    for invalid in ('foreign-repository', 'missing-lifecycle-diagnosis', 'foreign-occurrence', 'duplicate-occurrence', 'wrong-remedy', 'foreign-evidence'):
                        tampered = json.loads(body)
                        result = tampered['deterministic_results'][0]
                        if invalid == 'foreign-repository':
                            tampered['repository'] = 'another/project'
                        elif invalid == 'missing-lifecycle-diagnosis':
                            tampered['phase'] = 'outcome'
                        elif invalid == 'foreign-occurrence':
                            result['diagnosis']['occurrence_id'] = 'sha256:' + 'f' * 64
                        elif invalid == 'duplicate-occurrence':
                            tampered['deterministic_results'].append({**copy.deepcopy(result), 'row': 2, 'intended': 'release_alert'})
                        elif invalid == 'wrong-remedy':
                            result['row'] = 3
                        else:
                            result['diagnosis']['evidence'][0]['source_sha'] = 'f' * 40
                        with self.subTest(invalid=invalid), self.assertRaises(subprocess.CalledProcessError):
                            invoke('notify', 'execute', invalid, body=C.canonical(tampered).decode())
                    self.assertEqual((root / 'events').read_text().splitlines(),
                                     [json.dumps(event) for event in events])
                if case == 'unselected-release':
                    self.assertFalse(snapshot['failures'][0]['release_blocking'])
                    release = snapshot['failures'][1]
                    self.assertTrue(release['release_blocking'])
                    self.assertEqual(events[1]['failures'][1]['log_artifacts'], release['log_artifacts'])
                    self.assertEqual(events[1]['unselected_occurrences'], [release['occurrence_id']])
                    self.assertEqual(events[1]['unselected_reason'], 'diagnosis_reservation_exhausted')
                    self.assertFalse(any(s['operation']['kind'] == 'ci_diagnosis' for s in bundle['definitions'][bundle['root']]['steps']))
                for version in (1, 2, 3, 4):
                    old = json.loads(body)
                    old['schema'] = 'af.ci-health-notification/' + str(version)
                    with self.assertRaises(ValueError):
                        C.structural(old, 'ci-health-notification')

    def test_findings_batch_all_observed_failures_omissions_and_release_alerts_within_existing_bounds(self):
        spec = importlib.util.spec_from_file_location('ci_scenario', FIXTURE / 'configure.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        raw = service()
        raw['omissions'] = [str(i) + ' missing evidence ' + 'x' * 230 for i in range(100)]
        raw['jobs'] = [dict(raw['jobs'][0], run_id=i + 1, job_id=i + 1,
            job_name='核"\\' * 80 + str(i), release_blocking=True,
            log_artifacts=['sha256:' + format(j, '064x') for j in range(20)]) for i in range(100)]
        snapshot = self.observe(raw)
        self.assertTrue(snapshot['failures'])
        target = {'repo': '/fixture/repo', 'state': '/fixture/state', 'task_id': 'diagnose',
                  'plan_id': C.digest('plan'), 'revision_id': C.digest('revision'), 'max_tokens': 24096}
        bundle = module.bundle(snapshot, C.digest(snapshot), target, exhausted=True)
        steps = bundle['definitions'][bundle['root']]['steps']
        findings = [s for s in steps if s['id'].startswith('notify-findings')]
        self.assertGreater(len(findings), 1)
        self.assertLessEqual(len(steps), 20)
        messages = [json.loads(s['inputs']['body']['value']['value']) for s in findings]
        self.assertEqual([f for m in messages for f in m['failures']], C.failure_summaries(snapshot))
        self.assertEqual([o for m in messages for o in m['omissions']], snapshot['omissions'])
        self.assertEqual([a for m in messages for a in m['release_alerts']], [f['occurrence_id'] for f in snapshot['failures']])
        for message in messages:
            C.structural(message, 'ci-health-notification')
        for step in findings:
            inputs = {key: value['value'] for key, value in step['inputs'].items() if value['source'] == 'literal'}
            inputs['subject'] = {'type': 'text', 'value': C.digest(snapshot['repository'])}
            request = {'schema': 'af/scenario-adapter-request@4', 'operation': 'notify', 'method': 'reconcile',
                'intent_id': C.digest(step['id']), 'input_digest': C.digest(inputs),
                'destination': 'fixture:ci-health', 'inputs': inputs}
            self.assertLess(len(C.canonical(request)), 65536)
        # The request bound is not a new, smaller per-body limit. A large valid scope
        # that still fits the existing protocol must retain its complete identity.
        healthy = service()
        healthy['repository'] = 'r' * 20000 + '/project'
        healthy['jobs'][0]['conclusion'] = 'success'
        healthy['nightly_receipt']['conclusion'] = 'success'
        long_scope = self.observe(healthy)
        rendered = module.bundle(long_scope, C.digest(long_scope), target)
        first = rendered['definitions'][rendered['root']]['steps'][1]
        body = first['inputs']['body']['value']['value']
        self.assertNotIn('repository', json.loads(body))
        self.assertEqual(rendered['definitions'][rendered['root']]['steps'][1]['inputs']['subject'],
                         {'source': 'input', 'port': 'repository'})
        self.assertEqual(module.scope_inputs(long_scope)['repository']['value'], healthy['repository'])
        request_inputs = {'subject': module.scope_inputs(long_scope)['repository'],
                          'revision': {'type': 'text', 'value': long_scope['source_sha']},
                          'body': {'type': 'text', 'value': body}}
        request = {'schema': 'af/scenario-adapter-request@4', 'operation': 'notify', 'method': 'reconcile',
            'intent_id': C.digest('long-scope'), 'input_digest': C.digest(request_inputs),
            'destination': 'fixture:ci-health', 'inputs': request_inputs}
        self.assertGreater(len(C.canonical(request)), 16384)
        self.assertLessEqual(len(C.canonical(request)), 65536)
        request['inputs'] = {'subject': {'type': 'text', 'value': healthy['repository']},
            'revision': {'type': 'text', 'value': healthy['source_sha']},
            'body': {'type': 'text', 'value': body}}
        self.assertLess(len(C.canonical(request)), 65536)

    def test_healthy_missing_and_malformed_evidence_remain_distinct(self):
        healthy = service()
        healthy['jobs'][0]['conclusion'] = 'success'
        healthy['nightly_receipt']['conclusion'] = 'success'
        self.assertTrue(self.observe(healthy)['complete'])
        self.assertEqual(self.observe(healthy)['failures'], [])
        for key in ('jobs', 'nightly_receipt'):
            missing = service()
            del missing[key]
            snapshot = self.observe(missing)
            self.assertFalse(snapshot['complete'])
            self.assertTrue(snapshot['omissions'])
        self.journal.close()
        self.journal = C.Journal(Path(self.temp.name) / 'observer-malformed.sqlite')
        self.addCleanup(self.journal.close)
        for key in ('run_attempt', 'head_sha', 'job_id'):
            malformed = service()
            del malformed['jobs'][0][key]
            snapshot = self.observe(malformed)
            self.assertFalse(snapshot['complete'])
            self.assertEqual(snapshot['failures'], [])
        bad = service()
        del bad['source_sha']
        with self.assertRaisesRegex(ValueError, 'Missing source SHA'):
            self.observe(bad)
        self.observe()
        bad = service()
        bad['nightly_receipt']['identity'] = 'malformed receipt identity'
        bad['jobs'][0]['conclusion'] = 'success'
        with self.assertRaises(ValueError):
            self.observe(bad)
        bad['nightly_receipt'] = service()['nightly_receipt']
        for malformed_omissions in ('not an array', [None]):
            bad['omissions'] = malformed_omissions
            with self.assertRaises(ValueError):
                self.observe(bad)
        later = service()
        later['jobs'][0]['run_attempt'] += 1
        self.assertEqual(self.observe(later)['failures'][0]['occurrence_count'], 2,
                         'Malformed enclosing evidence must not close an active signature')

    def test_normalized_logs_share_signatures_and_preserve_assertions_tests_workflows_platforms(self):
        a = service()['jobs'][0]
        a.pop('failing_tests', None)
        a['error_lines'] = ['\x1b[31m2026-11-09T03:00:01Z error: assertion x == y /tmp/af-11/a in 1.2s sha ' + 'a' * 40 + ' run_id=12 job #3   \x1b[0m']
        b = copy.deepcopy(a)
        b.update(run_id=200, run_attempt=2, job_id=99, head_sha='b' * 40)
        b['error_lines'] = ['2026-11-10T09:01:02Z error: assertion x == y /private/tmp/af-99/b in 3.4s sha ' + 'b' * 40 + ' run_id=99 job #4']
        self.assertEqual(C.fingerprint(a), C.fingerprint(b))
        repository = 'octo/project'
        sig = lambda j: C.signature_id(repository, j, C.occurrence_id(repository, j), C.fingerprint(j))
        self.assertEqual(sig(a), sig(b))
        for key, value in [('workflow_file', '.github/workflows/other.yml'), ('platform', 'macos'), ('job_name', 'other')]:
            different = copy.deepcopy(b)
            different[key] = value
            self.assertNotEqual(sig(a), sig(different))
        b['error_lines'] = ['error: assertion x == z']
        self.assertNotEqual(sig(a), sig(b))
        a['failing_tests'] = ['module::z', 'module::a']
        self.assertEqual(C.fingerprint(a), 'test: module::a')
        self.assertIn('module::a', C.normalize('test module::a failed'))
        self.assertEqual(C.normalize('error: /private/var/folders/ab/random/T/a 2ms'), C.normalize('error: /var/folders/yz/other/T/b 4ms'))
        self.assertEqual(C.normalize('\x1b]8;;https://example.test\x07error\x1b]8;;\x07'), 'error')
        self.assertEqual(C.normalize('commit abcdef1 in 2 minutes'), C.normalize('commit 1234567 in 4 minutes'))

    def test_unknown_signatures_are_unique_per_occurrence_and_cannot_gain_model_category(self):
        raw = service()
        raw['jobs'][0].update(failing_tests=[], error_lines=[])
        first = self.observe(raw)
        raw['jobs'][0]['run_attempt'] += 1
        second = self.observe(raw)
        self.assertNotEqual(first['failures'][0]['signature_id'], second['failures'][0]['signature_id'])
        result, dispatched = C.diagnose(C.digest(first), first, first['failures'][0], self.diagnosis(first))
        self.assertFalse(dispatched)
        self.assertEqual(result['category'], 'unknown')
        with self.assertRaisesRegex(ValueError, 'Unknown signature'):
            C.validate_diagnosis(self.diagnosis(first), C.digest(first), first)

    def test_repeated_attempts_cross_head_history_expiry_and_green_reset(self):
        raw = service()
        raw['jobs'][0]['completed_at'] = '2026-11-09T01:00:00Z'
        first = self.observe(raw)['failures'][0]
        self.assertEqual(self.observe(raw)['failures'][0]['occurrence_count'], 1)
        raw['jobs'][0]['job_id'] += 1
        self.assertEqual(self.observe(raw)['failures'][0]['occurrence_count'], 1)
        raw['jobs'][0]['run_attempt'] += 1
        observed = self.observe(raw)
        self.assertEqual(next(f for f in observed['failures'] if f['run_attempt'] == 2)['occurrence_count'], 2)
        raw['jobs'][0].update(head_sha='b' * 40, run_id=300)
        changed = next(f for f in self.observe(raw)['failures'] if f['run_id'] == 300)
        self.assertEqual(changed['signature_id'], first['signature_id'])
        self.assertEqual(changed['occurrence_count'], 3)
        raw['jobs'][0]['conclusion'] = 'success'
        raw['jobs'][0]['completed_at'] = '2026-11-09T02:00:00Z'
        other_head_success = copy.deepcopy(raw['jobs'][0])
        other_head_success.update(head_sha='a' * 40, job_id=999)
        raw['jobs'].append(other_head_success)
        self.assertEqual(self.observe(raw)['failures'], [])
        raw['jobs'][0]['conclusion'] = 'failure'
        raw['jobs'].pop()
        self.assertEqual(self.observe(raw)['failures'][0]['occurrence_count'], 1)
        raw['captured_at'] = '2026-11-24T04:00:00Z'
        expired = self.observe(raw)['failures'][0]
        self.assertEqual(expired['occurrence_count'], 1)
        self.assertEqual(expired['first_seen_at'], raw['captured_at'])
        self.journal.close()
        self.journal = C.Journal(Path(self.temp.name) / 'observer.sqlite')
        self.addCleanup(self.journal.close)
        self.assertEqual(self.observe(raw)['failures'][0]['occurrence_count'], 1)

    def test_journal_capture_receipt_schema_replays_counts_after_reopen_and_refuses_old_or_malformed_state(self):
        raw = service()
        raw['jobs'].append({**raw['jobs'][0], 'run_id': 302, 'job_id': 404})
        captured = self.observe(raw)
        self.assertEqual(sorted(f['occurrence_count'] for f in captured['failures']), [1, 2])
        record = self.journal.metadata()
        C.structural(record, 'ci-health-state')
        self.assertEqual(record, {'schema': C.STATE, 'capture': {'service_digest': C.digest(raw), 'snapshot': captured}})
        # The writer's exact bytes satisfy the shipped schema through the strict reader.
        schema = C.loads((ROOT / 'schemas/ci-health-state-v2.json').read_bytes())
        validator = type(C._VALIDATOR)()
        validator.schemas[schema['$id']] = schema
        self.assertEqual(validator.errors(record, schema['$id']), [])
        self.journal.close()
        self.journal = C.Journal(Path(self.temp.name) / 'observer.sqlite')
        self.addCleanup(self.journal.close)
        self.assertEqual(self.observe(raw), captured)
        raw['captured_at'] = '2026-11-10T04:00:00Z'
        later = self.observe(raw)
        self.assertEqual([f['occurrence_count'] for f in later['failures']], [2, 2])
        self.assertEqual(self.observe(raw), later)
        self.assertEqual(self.journal.metadata()['capture'], {'service_digest': C.digest(raw), 'snapshot': later})
        for state in ('af.ci-health-state/1', {'schema': 'af.ci-health-state/1', 'capture': None},
                      {**record, 'extra': True}, {**record, 'capture': {'service_digest': 'bad', 'snapshot': captured}},
                      {**record, 'capture': {**record['capture'], 'snapshot': {**captured, 'schema': 'af.ci-health-snapshot/0'}}}):
            self.assertTrue(validator.errors(state, schema['$id']))
            path = Path(self.temp.name) / 'refused.sqlite'
            with closing(C.sqlite3.connect(path)) as db, db:
                db.execute('CREATE TABLE IF NOT EXISTS metadata (schema TEXT NOT NULL)')
                db.execute('DELETE FROM metadata')
                db.execute('INSERT INTO metadata VALUES (?)', (state if isinstance(state, str) else C.canonical(state).decode(),))
            before = path.read_bytes()
            with self.subTest(state=state), self.assertRaisesRegex(ValueError, 'observer state'):
                C.Journal(path)
            self.assertEqual(path.read_bytes(), before)

    def test_capture_receipt_and_recurrence_folds_roll_back_together_on_refusal(self):
        captured = self.observe()
        before = self.journal.metadata()
        rows = self.journal.db.execute('SELECT * FROM signatures').fetchall()
        raw = service()
        raw['jobs'][0].update(run_id=302, job_id=404)
        for expected in (C.digest(captured), C.digest('different snapshot')):
            with self.assertRaisesRegex(ValueError, 'Exact snapshot precondition refused'):
                C.observe(raw, self.journal, expected_snapshot=expected)
            self.assertEqual(self.journal.metadata(), before)
            self.assertEqual(self.journal.db.execute('SELECT * FROM signatures').fetchall(), rows)
        self.journal.db.execute("CREATE TRIGGER refuse_receipt BEFORE UPDATE ON metadata BEGIN SELECT RAISE(ABORT, 'receipt refused'); END")
        with self.assertRaisesRegex(C.sqlite3.IntegrityError, 'receipt refused'):
            self.observe(raw)
        self.assertEqual(self.journal.metadata(), before)
        self.assertEqual(self.journal.db.execute('SELECT * FROM signatures').fetchall(), rows)
        self.journal.db.execute('DROP TRIGGER refuse_receipt')
        self.assertEqual(self.observe(), captured)

    def test_green_closure_preserves_other_heads_recurrence_and_release_alerts(self):
        raw = service()
        main = copy.deepcopy(raw['jobs'][0])
        main.update(head_sha='a' * 40, completed_at='2026-11-09T01:00:00Z')
        release = copy.deepcopy(main)
        release.update(head_sha='b' * 40, job_id=404, run_attempt=2,
                       completed_at='2026-11-09T02:00:00Z', release_blocking=True)
        raw['release_pr'] = {'number': 7, 'head_sha': 'b' * 40}
        self.observe(dict(raw, jobs=[main, release]))
        success = copy.deepcopy(main)
        success.update(conclusion='success', job_id=405, run_attempt=3,
                       completed_at='2026-11-09T03:00:00Z')
        observed = self.observe(dict(raw, jobs=[success]))
        self.assertEqual([f['head_sha'] for f in observed['failures']], ['b' * 40])
        self.assertTrue(observed['failures'][0]['release_blocking'])
        release['run_attempt'] = 4
        release['job_id'] = 406
        release['completed_at'] = '2026-11-09T04:00:00Z'
        observed = self.observe(dict(raw, jobs=[release]))
        self.assertEqual(observed['failures'][0]['occurrence_count'], 3)
        self.assertTrue(observed['failures'][0]['release_blocking'])

    def test_unresolved_failures_survive_incomplete_captures_but_expire_and_follow_head_selectors(self):
        raw = service()
        failure = copy.deepcopy(raw['jobs'][0])
        failure.update(completed_at='2026-11-09T03:20:00Z', release_blocking=True)
        self.observe(dict(raw, jobs=[failure]))
        unavailable = self.observe(dict(raw, jobs=[]))
        self.assertFalse(unavailable['complete'])
        self.assertEqual([f['job_id'] for f in unavailable['failures']], [failure['job_id']])
        success = copy.deepcopy(failure)
        success.update(conclusion='success', job_id=202, completed_at='2026-11-09T03:10:00Z')
        older = self.observe(dict(raw, jobs=[success]))
        self.assertFalse(older['complete'])
        self.assertEqual([f['job_id'] for f in older['failures']], [failure['job_id']])
        success['completed_at'] = '2026-11-09T03:30:00Z'
        raw['nightly_receipt']['conclusion'] = 'success'
        closed = self.observe(dict(raw, jobs=[success]))
        self.assertEqual(closed['failures'], [])
        self.assertTrue(closed['complete'], closed['omissions'])

        # Expiry and head advancement never resurrect retained evidence.
        raw['captured_at'] = '2026-11-30T04:00:00Z'
        raw['nightly_receipt']['conclusion'] = 'success'
        expired = self.observe(dict(raw, jobs=[success]))
        self.assertEqual(expired['failures'], [])
        self.assertTrue(expired['complete'], expired['omissions'])
        raw['captured_at'] = '2026-11-10T04:00:00Z'
        raw['source_sha'] = 'd' * 40
        superseded = self.observe(dict(raw, jobs=[success]))
        self.assertEqual(superseded['failures'], [])
        self.assertTrue(superseded['complete'], superseded['omissions'])

    def test_malformed_non_object_jobs_are_isolated_before_sorting(self):
        raw = service()
        valid = copy.deepcopy(raw['jobs'][0])
        valid['release_blocking'] = True
        for jobs in ([valid, None, 'text', []], [None, 'text', [], valid]):
            observed = self.observe(dict(raw, jobs=jobs))
            self.assertFalse(observed['complete'])
            failure, = observed['failures']
            self.assertEqual(failure['job_id'], valid['job_id'])
            self.assertTrue(failure['release_blocking'])

    def test_two_later_successes_cannot_restore_an_already_closed_failure(self):
        raw = service()
        raw['nightly_receipt']['conclusion'] = 'success'
        failure = copy.deepcopy(raw['jobs'][0])
        failure['completed_at'] = '2026-11-09T01:00:00Z'
        self.observe(dict(raw, jobs=[failure]))
        first = copy.deepcopy(failure)
        first.update(conclusion='success', job_id=202, completed_at='2026-11-09T02:00:00Z')
        second = copy.deepcopy(failure)
        second.update(conclusion='success', job_id=203, completed_at='2026-11-09T03:00:00Z')
        observed = self.observe(dict(raw, jobs=[first, second]))
        self.assertEqual(observed['failures'], [])
        self.assertTrue(observed['complete'], observed['omissions'])
        self.assertEqual(self.observe(dict(raw, jobs=[first, second])), observed)

    def test_green_closure_is_per_signature_and_never_erases_an_unresolved_sibling(self):
        raw = service()
        first = copy.deepcopy(raw['jobs'][0])
        second = copy.deepcopy(first)
        first.update(completed_at='2026-11-09T01:00:00Z', failing_tests=['test:one'], release_blocking=True)
        second.update(completed_at='2026-11-09T03:00:00Z', job_id=404, run_attempt=2,
                      failing_tests=['test:two'], error_lines=['error: different failing test'], release_blocking=True)
        raw['jobs'] = [first, second]
        self.observe(raw)
        success = copy.deepcopy(first)
        success.update(conclusion='success', run_attempt=3, job_id=405,
                       completed_at='2026-11-09T02:00:00Z')
        observed = self.observe(dict(raw, jobs=[success]))
        self.assertFalse(observed['complete'])
        self.assertEqual([f['job_id'] for f in observed['failures']], [404])
        self.assertTrue(observed['failures'][0]['release_blocking'])
        self.assertTrue(any('another failure' in reason for reason in observed['omissions']))

        # The same ordering rule holds when both failures and the intervening success share one capture.
        self.journal.close()
        self.journal = C.Journal(Path(self.temp.name) / 'observer-sibling.sqlite')
        self.addCleanup(self.journal.close)
        observed = self.observe(dict(raw, jobs=[first, success, second]))
        self.assertFalse(observed['complete'])
        self.assertEqual([f['job_id'] for f in observed['failures']], [404])

    def test_green_closure_is_order_independent_within_a_capture_and_conflicts_are_unknown(self):
        raw = service()
        raw['jobs'][0]['completed_at'] = '2026-11-09T01:00:00Z'
        passed = copy.deepcopy(raw['jobs'][0])
        passed.update(conclusion='success', run_attempt=2, job_id=202,
                      completed_at='2026-11-09T02:00:00Z')
        raw['nightly_receipt']['conclusion'] = 'success'
        for jobs in ([raw['jobs'][0], passed], [passed, raw['jobs'][0]]):
            observed = self.observe(dict(raw, jobs=jobs))
            self.assertTrue(observed['complete'])
            self.assertEqual(observed['failures'], [])
        raw['jobs'][0].update(run_attempt=3, job_id=203)
        self.assertEqual(self.observe(raw)['failures'][0]['occurrence_count'], 1)
        conflict = copy.deepcopy(raw['jobs'][0])
        conflict['conclusion'] = 'success'
        observed = self.observe(dict(raw, jobs=[raw['jobs'][0], conflict]))
        self.assertFalse(observed['complete'])
        self.assertTrue(any('Conflicting conclusions' in item for item in observed['omissions']))

    def test_diagnosis_rejects_schema_identity_and_foreign_or_empty_evidence(self):
        snapshot = self.observe()
        diagnosis = self.diagnosis(snapshot)
        C.validate_diagnosis(diagnosis, C.digest(snapshot), snapshot)
        for path, value in [('schema', 'af.ci-diagnosis/0'), ('snapshot_id', 'sha256:' + 'f' * 64),
            ('source_sha', 'f' * 40), ('occurrence_id', 'sha256:' + 'f' * 64),
            ('signature_id', 'sha256:' + 'f' * 64), ('evidence', []), ('category', {'value': 'transient'})]:
            bad = copy.deepcopy(diagnosis)
            bad[path] = value
            with self.subTest(path=path), self.assertRaises(ValueError):
                C.validate_diagnosis(bad, C.digest(snapshot), snapshot)
            result, task = C.diagnose(C.digest(snapshot), snapshot, snapshot['failures'][0], bad)
            self.assertTrue(task)
            self.assertEqual(result['category'], 'unknown')
            self.assertTrue(result['rationale'].startswith('malformed evidence: '))
        for key in ('snapshot_id', 'source_sha', 'reference'):
            bad = copy.deepcopy(diagnosis)
            bad['evidence'][0][key] = 'foreign'
            with self.assertRaises(ValueError):
                C.validate_diagnosis(bad, C.digest(snapshot), snapshot)

    def test_another_occurrence_of_the_same_signature_does_not_satisfy_the_selection(self):
        raw = service()
        other = dict(raw['jobs'][0], run_attempt=2, job_id=999)
        snapshot = self.observe(dict(raw, jobs=[raw['jobs'][0], other]))
        first, second = snapshot['failures']
        self.assertEqual(first['signature_id'], second['signature_id'])
        answer = C.malformed(C.digest(snapshot), snapshot, second, 'fixture')
        answer.update(category='test_bug', rationale='fixture classified another occurrence')
        result, invoked = C.diagnose(C.digest(snapshot), snapshot, first, answer)
        self.assertTrue(invoked)
        self.assertEqual(result['category'], 'unknown')
        self.assertIn('different selected occurrence', result['rationale'])

    def test_exact_infrastructure_and_transient_precede_diagnosis_task(self):
        snapshot = self.observe()
        f = snapshot['failures'][0]
        for category, flags in [('ci_infra', {'infrastructure': [f['signature_id']]}),
                                ('transient', {'known_transients': [f['signature_id']]})]:
            result, task = C.diagnose(C.digest(snapshot), snapshot, f, None, **flags)
            self.assertFalse(task)
            self.assertEqual(result['category'], category)
        result, task = C.diagnose(C.digest(snapshot), snapshot, f, self.diagnosis(snapshot))
        self.assertTrue(task)
        self.assertEqual(result['category'], 'product_bug')

    def test_quarantine_intent_requires_two_distinct_recent_inventory_resolved_tasks_on_the_same_platform(self):
        evidence = json.loads((FIXTURE / 'gate-evidence.json').read_text())
        snapshot = self.observe()
        failure = snapshot['failures'][0]
        count = lambda value: C.reviewed_gate_count(value['documents'], value['inventory'], failure,
            snapshot['captured_at'], value['evidence_platform'])
        self.assertEqual(count(evidence), 2)
        duplicate = copy.deepcopy(evidence)
        duplicate['documents'][0]['tasks'][1]['task_id'] = 'gate-1'
        self.assertEqual(count(duplicate), 0)
        stale = copy.deepcopy(evidence)
        stale['documents'][0]['tasks'][1]['updated_unix_ms'] -= 8 * 86400000
        self.assertEqual(count(stale), 0)
        wrong_platform = copy.deepcopy(evidence)
        wrong_platform['evidence_platform'] = 'macos'
        self.assertEqual(count(wrong_platform), 0)
        unknown = copy.deepcopy(evidence)
        for row in unknown['documents'][0]['tasks']:
            row['gate']['flaky'] = ['gate-store-regression', 'a model says flaky']
        self.assertEqual(count(unknown), 0)
        ignored = copy.deepcopy(evidence)
        for suite in ignored['inventory']['rust-suites'].values():
            for test in suite['testcases'].values():
                test['ignored'] = True
        with self.assertRaisesRegex(ValueError, 'ignored-test policy drift'):
            count(ignored)

    def test_every_overlapping_pair_of_remedy_predicates_chooses_first_row(self):
        snapshot = self.observe()
        f = snapshot['failures'][0]
        diagnosis = self.diagnosis(snapshot)
        pairs = set()
        # Exhaust the full boolean product, not a few selected non-overlapping examples.
        for bits in itertools.product((False, True), repeat=8):
            complete, known, release, exhausted, reserved, eligible, repair, opened = bits
            for category in C.CATEGORIES:
                snapshot.update(complete=complete)
                f.update(normalized_fingerprint='assert x' if known else None, release_blocking=release)
                diagnosis['category'] = category
                predicates = [not complete or not known or category == 'unknown' or exhausted,
                    release, category == 'ci_infra', category == 'transient' and not reserved,
                    category == 'test_bug' and eligible,
                    category in ('product_bug', 'test_bug') and repair and not opened, True]
                expected = next(i + 1 for i, yes in enumerate(predicates) if yes)
                row, _ = C.remedy(snapshot, f, diagnosis, exhausted=exhausted, rerun_reserved=reserved,
                    eligible_gate_occurrences=2 if eligible else 1, small_repair=repair, open_pr=opened)
                self.assertEqual(row, expected)
                pairs.update((a + 1, b + 1) for a, b in itertools.combinations(range(7), 2)
                             if predicates[a] and predicates[b])
        self.assertEqual(pairs, {(1, 2), (1, 3), (1, 4), (1, 5), (1, 6), (1, 7),
            (2, 3), (2, 4), (2, 5), (2, 6), (2, 7), (3, 7), (4, 7), (5, 6), (5, 7), (6, 7)})

    def test_evidence_bounds_are_named_omissions_not_healthy_checks(self):
        raw = service()
        raw['jobs'] *= 101
        self.assertFalse(self.observe(raw)['complete'])
        raw = service()
        raw['jobs'][0]['log_artifacts'] = ['sha256:' + format(i, '064x') for i in range(21)]
        snapshot = self.observe(raw)
        self.assertFalse(snapshot['complete'])
        self.assertEqual(len(snapshot['failures'][0]['log_artifacts']), 20)
        raw = service()
        raw['jobs'][0]['evidence_unreadable'] = True
        self.assertFalse(self.observe(raw)['complete'])
        raw = service()
        raw['jobs'] = [dict(raw['jobs'][0], run_id=i + 1, job_id=i + 1,
                           error_lines=['assertion: ' + 'x' * 1800], failing_tests=[]) for i in range(100)]
        snapshot = self.observe(raw)
        self.assertLessEqual(len(C.canonical(snapshot)), 65536)
        self.assertFalse(snapshot['complete'])
        self.assertIn('64 KiB evidence bound exceeded', snapshot['omissions'])


class Response(io.BytesIO):
    def __init__(self, status, body, headers=None):
        super().__init__(body)
        self.status, self.headers = status, headers or {}


class Transport:
    """Only an in-memory opener: no socket, CLI, token service or GitHub fixture extension."""
    def __init__(self, routes, token=None):
        self.routes, self.token, self.calls, self.responses = routes, token, [], []

    def open(self, request, timeout):
        self.calls.append((request.get_method(), request.full_url, timeout))
        api = request.full_url.startswith('https://api.github.com/repos/octo/project/')
        if request.get_method() != 'GET' or not (api or request.full_url in self.routes):
            raise AssertionError('unexpected remote effect or destination')
        expected = 'Bearer ' + self.token if self.token and api else None
        if request.get_header('Authorization') != expected:
            raise AssertionError('unexpected credential delivery')
        route = request.full_url[len('https://api.github.com/'):] if api else request.full_url
        value = self.routes[route]
        if isinstance(value, Exception):
            raise value
        if callable(value):
            value = value()
        parts = value if isinstance(value, tuple) else (200, C.canonical(value))
        response = Response(*parts)
        self.responses.append(response)
        return response


def production_routes():
    prefix = 'repos/octo/project/'
    routes = {prefix + 'commits/main': {'sha': 'a' * 40}, prefix + 'pulls/7': {
        'number': 7, 'state': 'open', 'base': {'repo': {'full_name': 'octo/project'}}, 'head': {'sha': 'd' * 40}}}
    for sha, run, job, conclusion, labels in [('a' * 40, 101, 201, 'success', ['ubuntu-latest']),
                                            ('d' * 40, 301, 403, 'failure', ['macos-14', 'ARM64'])]:
        check = job + 1000
        routes[prefix + f'commits/{sha}/check-runs?filter=latest&per_page=100'] = {
            'total_count': 1, 'check_runs': [{'id': check, 'head_sha': sha, 'name': 'validate',
                'status': 'completed', 'conclusion': conclusion, 'output': {'annotations_count': 0}}]}
        routes[prefix + f'actions/runs?head_sha={sha}&per_page=100'] = {'total_count': 1, 'workflow_runs': [{
            'id': run, 'workflow_id': 1, 'run_attempt': 2, 'head_sha': sha, 'status': 'completed',
            'path': '.github/workflows/validate.yml', 'created_at': '2026-11-09T03:00:00Z',
            'repository': {'full_name': 'octo/project'}}]}
        routes[prefix + f'actions/runs/{run}/attempts/2'] = {**copy.deepcopy(routes[prefix + f'actions/runs?head_sha={sha}&per_page=100']['workflow_runs'][0]),
            'run_started_at': '2026-11-09T03:00:00Z', 'updated_at': '2026-11-09T03:01:00Z'}
        routes[prefix + f'actions/runs/{run}/attempts/2/jobs?per_page=100'] = {'total_count': 1, 'jobs': [{
            'id': job, 'name': 'validate', 'status': 'completed', 'conclusion': conclusion,
            'run_id': run, 'run_attempt': 2, 'head_sha': sha, 'labels': labels,
            'completed_at': '2026-11-09T03:01:00Z',
            'check_run_url': f'https://api.github.com/{prefix}check-runs/{check}'}]}
        if conclusion == 'failure':
            routes[prefix + f'check-runs/{check}/annotations?per_page=100'] = []
            routes[prefix + f'actions/jobs/{job}/logs'] = (200, b'2026-11-09T03:01:00Z error: assertion x == y\n')
    return routes


def add_production_job(routes, sha, run, attempt, job, conclusion, started, updated,
                       name='validate', labels=None):
    """Add one exact check/run/job triple with its own attempt completion interval."""
    prefix = 'repos/octo/project/'
    check = job + 1000
    checks = routes[prefix + f'commits/{sha}/check-runs?filter=latest&per_page=100']
    checks['check_runs'].append({'id': check, 'head_sha': sha, 'name': name,
        'status': 'completed', 'conclusion': conclusion, 'output': {'annotations_count': 0}})
    checks['total_count'] = len(checks['check_runs'])
    runs = routes[prefix + f'actions/runs?head_sha={sha}&per_page=100']['workflow_runs']
    if not any(item['id'] == run for item in runs):
        runs.append({'id': run, 'workflow_id': 1, 'run_attempt': attempt, 'head_sha': sha,
            'status': 'completed', 'path': '.github/workflows/validate.yml', 'created_at': started,
            'repository': {'full_name': 'octo/project'}})
        routes[prefix + f'actions/runs?head_sha={sha}&per_page=100']['total_count'] = len(runs)
    routes[prefix + f'actions/runs/{run}/attempts/{attempt}'] = {
        'id': run, 'workflow_id': 1, 'run_attempt': attempt, 'head_sha': sha,
        'status': 'completed', 'path': '.github/workflows/validate.yml', 'created_at': started,
        'run_started_at': started, 'updated_at': updated,
        'repository': {'full_name': 'octo/project'}}
    route = prefix + f'actions/runs/{run}/attempts/{attempt}/jobs?per_page=100'
    jobs = routes.setdefault(route, {'total_count': 0, 'jobs': []})
    jobs['jobs'].append({'id': job, 'name': name, 'status': 'completed', 'conclusion': conclusion,
        'run_id': run, 'run_attempt': attempt, 'head_sha': sha,
        'labels': labels or ['ubuntu-latest'], 'completed_at': updated,
        'check_run_url': f'https://api.github.com/{prefix}check-runs/{check}'})
    jobs['total_count'] = len(jobs['jobs'])
    if conclusion == 'failure':
        routes[prefix + f'check-runs/{check}/annotations?per_page=100'] = []
        routes[prefix + f'actions/jobs/{job}/logs'] = (200, b'2026-11-09T03:31:00Z error: assertion x == y\n')


def near_limit_routes(fingerprint='核"\\' * 675):
    """Eight jobs fit the snapshot cap but overflow the escaped native response."""
    routes = production_routes()
    prefix = 'repos/octo/project/'
    jobs = routes[prefix + 'actions/runs/301/attempts/2/jobs?per_page=100']
    checks = routes[prefix + 'commits/' + 'd' * 40 + '/check-runs?filter=latest&per_page=100']
    job, check = jobs['jobs'][0], checks['check_runs'][0]
    jobs.update(total_count=8, jobs=[])
    checks.update(total_count=8, check_runs=[])
    for i in range(8):
        job_id, check_id = 403 + i, 1403 + i
        name = '核"\\' * 80 + str(i)
        jobs['jobs'].append({**job, 'id': job_id, 'name': name,
            'check_run_url': f'https://api.github.com/{prefix}check-runs/{check_id}'})
        checks['check_runs'].append({**check, 'id': check_id, 'name': name,
            'output': {'annotations_count': 1}})
        routes[prefix + f'check-runs/{check_id}/annotations?per_page=100'] = [
            {'annotation_level': 'failure', 'message': fingerprint}]
        routes[prefix + f'actions/jobs/{job_id}/logs'] = (200, b'error: bounded log\n')
    return routes


def hermetic_production_adapter(production, options, routes, notifications):
    """Install the real production observer plus the existing fixture notifier, without sockets.

    All transport/helper bytes are embedded; the executable imports no checkout code.
    The native Scenario still reads and validates the actual production wire response.
    """
    return ('#!/usr/bin/env python3\nimport io,json,sys,types,urllib.request\n'
        + 'C=types.SimpleNamespace(canonical=lambda value:json.dumps(value,sort_keys=True,separators=(",",":"),ensure_ascii=False).encode())\n'
        + inspect.getsource(Response) + '\n' + inspect.getsource(Transport) + '\n'
        + 'raw=sys.stdin.buffer.read(65537)\nrequest=json.loads(raw)\n'
        + 'sys.stdin=io.TextIOWrapper(io.BytesIO(raw))\n'
        + 'if request["operation"]=="observe":\n'
        + ' transport=Transport(' + repr(routes) + ')\n'
        + ' urllib.request.OpenerDirector.open=lambda self,req,timeout:transport.open(req,timeout)\n'
        + ' exec(' + repr(production.adapter_bytes(options).decode()) + ',{"__file__":__file__,"__name__":"__main__"})\n'
        + 'else:\n exec(' + repr(notifications.decode()) + ',{"__file__":__file__,"__name__":"__main__"})\n').encode()


class ProductionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.P = importlib.util.module_from_spec(spec := importlib.util.spec_from_file_location(
            'ci_health_production', ROOT / 'scripts/ci-health-github.py'))
        spec.loader.exec_module(cls.P)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.publish = json.loads((ROOT / 'fixtures/performance/schema/nightly-publish/valid-published.json').read_text())
        old = self.publish['tag']
        self.publish.update(repository='octo/project', head_sha='a' * 40, date='2026-11-09', tag='nightly-2026.11.09',
            published_unix_ms=int(C.instant('2026-11-09T03:30:00Z').timestamp() * 1000))
        self.publish['release']['url'] = 'https://github.com/octo/project/releases/tag/' + self.publish['tag']
        for asset in self.publish['assets']:
            asset['name'] = asset['name'].replace(old, self.publish['tag'])
        self.receipt = self.root / 'publish.json'
        self.receipt.write_bytes(C.canonical(self.publish))
        self.options = {'repository': 'octo/project', 'main_ref': 'main', 'since': '2026-11-08T04:00:00Z',
            'captured_at': '2026-11-09T04:00:00Z', 'release_pr': 7, 'nightly_receipt': str(self.receipt),
            'state': str(self.root / 'observer.sqlite'), 'github_token_file': None,
            'max_requests': 16, 'max_wall_seconds': 60, 'destination': 'github:octo/project'}

    def staged_options(self):
        directory = self.root / 'occurrences'
        directory.mkdir(mode=0o700, exist_ok=True)
        return {**{k: v for k, v in self.options.items()
                   if k not in ('since', 'captured_at', 'nightly_receipt', 'release_pr')},
                'occurrence_dir': str(directory)}

    def install_occurrence(self, snapshot=None, **changes):
        if snapshot is None:
            snapshot = self.P.observe({**self.options, 'state': str(self.root / 'expected.sqlite')},
                                      opener=Transport(production_routes()))
        document = C.occurrence_document(snapshot, since=self.options['since'], nightly_receipt=self.receipt)
        document.update(changes)
        directory = Path(self.staged_options()['occurrence_dir'])
        path = directory / (C.digest(snapshot)[7:] + '.json')
        path.write_bytes(C.canonical(document))
        path.chmod(0o600)
        return snapshot, path

    def request(self, **changes):
        snapshot, _ = self.install_occurrence()
        inputs = {key: {'type': 'text', 'value': value} for key, value in {
            'subject': 'octo/project', 'revision': 'a' * 40, 'condition': 'ci_observe_v1:' + C.digest(snapshot)}.items()}
        return {'schema': 'af/scenario-adapter-request@4', 'method': 'observe', 'operation': 'observe',
            'destination': self.options['destination'], 'intent_id': C.digest('observe'),
            'input_digest': C.digest(inputs), 'inputs': inputs, **changes}

    def test_production_observation_reads_real_rest_shapes_and_receipt_with_exact_release_head(self):
        transport = Transport(production_routes())
        snapshot = self.P.observe(self.options, opener=transport)
        C.validate_snapshot(snapshot)
        self.assertTrue(snapshot['complete'], snapshot['omissions'])
        self.assertEqual(snapshot['source_sha'], 'a' * 40)
        self.assertEqual(snapshot['release_pr'], {'number': 7, 'head_sha': 'd' * 40})
        failure, = snapshot['failures']
        self.assertEqual((failure['head_sha'], failure['run_id'], failure['run_attempt'], failure['job_id']), ('d' * 40, 301, 2, 403))
        self.assertEqual(failure['platform'], 'ARM64,macos-14')
        self.assertEqual(failure['normalized_fingerprint'], 'error: assertion x == y')
        self.assertTrue(failure['release_blocking'])
        self.assertEqual(snapshot['nightly_receipt'], {'identity': C.digest(self.publish), 'conclusion': 'success'})
        self.assertEqual(failure['log_artifacts'], ['sha256:' + __import__('hashlib').sha256(
            production_routes()['repos/octo/project/actions/jobs/403/logs'][1]).hexdigest()])
        self.assertEqual(len(transport.calls), 12)
        self.assertTrue(all(method == 'GET' and 0 < timeout <= 30 for method, _, timeout in transport.calls))
        self.assertTrue(all(response.closed for response in transport.responses))
        self.assertEqual(self.P.observe(self.options, opener=Transport(production_routes())), snapshot)
        # A rerun belongs to its selected attempt's window, not the original run's creation.
        rerun = production_routes()
        route = 'repos/octo/project/actions/runs?head_sha=' + 'd' * 40 + '&per_page=100'
        rerun[route]['workflow_runs'][0].update(created_at='2026-11-07T03:00:00Z',
            run_started_at='2026-11-09T03:00:00Z', updated_at='2026-11-09T03:01:00Z')
        rerun['repos/octo/project/actions/runs/301/attempts/2']['created_at'] = '2026-11-07T03:00:00Z'
        transport = Transport(rerun)
        observed = self.P.observe(self.options, opener=transport)
        self.assertEqual(observed['failures'], snapshot['failures'])
        self.assertTrue(observed['complete'], observed['omissions'])

    def test_production_check_annotations_are_bounded_fallback_evidence_when_logs_are_unavailable(self):
        routes = production_routes()
        annotation = {'annotation_level': 'failure', 'message': 'test fixture::exact ... FAILED'}
        routes['repos/octo/project/check-runs/1403/annotations?per_page=100'] = [annotation]
        routes['repos/octo/project/commits/' + 'd' * 40 + '/check-runs?filter=latest&per_page=100']['check_runs'][0]['output']['annotations_count'] = 1
        routes['repos/octo/project/actions/jobs/403/logs'] = (404, b'unavailable')
        transport = Transport(routes)
        snapshot = self.P.observe(self.options, opener=transport)
        self.assertTrue(snapshot['complete'], snapshot['omissions'])
        self.assertEqual(snapshot['failures'][0]['log_artifacts'], [C.digest([annotation])])
        self.assertEqual(snapshot['failures'][0]['normalized_fingerprint'], 'test: fixture::exact')
        self.assertTrue(any('/logs' in url for _, url, _ in transport.calls))
        # Exactly the declared bound is complete; a higher declared total is not.
        routes['repos/octo/project/check-runs/1403/annotations?per_page=100'] = [annotation] * 100
        check = routes['repos/octo/project/commits/' + 'd' * 40 + '/check-runs?filter=latest&per_page=100']['check_runs'][0]
        check['output']['annotations_count'] = 100
        exact = self.P.observe(self.options, opener=Transport(routes))
        self.assertTrue(exact['complete'], exact['omissions'])
        self.assertEqual(exact['failures'][0]['normalized_fingerprint'], 'test: fixture::exact')
        check['output']['annotations_count'] = 101
        self.assertFalse(self.P.observe(self.options, opener=Transport(routes))['complete'])
        # Retain the representative line, so a complete bounded read of many annotations
        # cannot overflow the existing 1 MiB service document before snapshot admission.
        check['output']['annotations_count'] = 100
        collected = self.P.collect(self.P.Github('octo/project', opener=Transport(routes)),
            captured_at=self.options['captured_at'], main_ref='main', since=self.options['since'],
            nightly_receipt=self.receipt, release_pr=7)
        self.assertEqual(collected['jobs'][1]['error_lines'], [annotation['message']])

    def test_generic_and_multiline_annotations_cannot_hide_named_log_test_identities(self):
        prefix = 'repos/octo/project/'
        signature_ids = []
        for name in ('a::first', 'z::last'):
            for message in ('Process completed with exit code 101.', 'ERROR: generic runner error',
                            'FAIL: generic runner failure', 'FAILED compilation',
                            'ERROR: command failed (timeout)', 'FAIL: compilation failed (rustc)'):
                routes = production_routes()
                annotations = [{'annotation_level': 'failure', 'message': message}]
                routes[prefix + 'check-runs/1403/annotations?per_page=100'] = annotations
                routes[prefix + 'commits/' + 'd' * 40 + '/check-runs?filter=latest&per_page=100']['check_runs'][0]['output']['annotations_count'] = 1
                routes[prefix + 'actions/jobs/403/logs'] = (200, f'error: generic harness error\ntest {name} ... FAILED\n'.encode())
                observed = self.P.observe(self.options, opener=Transport(routes))
                self.assertTrue(observed['complete'], observed['omissions'])
                failure, = observed['failures']
                self.assertEqual(failure['normalized_fingerprint'], 'test: ' + name)
                if message.startswith('Process'):
                    signature_ids.append(failure['signature_id'])
        self.assertNotEqual(*signature_ids)
        annotations[0]['message'] = 'error: generic harness error\ntest z::last ... FAILED\ntest a::first ... FAILED'
        transport = Transport(routes)
        observed = self.P.observe(self.options, opener=transport)
        self.assertTrue(observed['complete'])
        self.assertEqual(observed['failures'][0]['normalized_fingerprint'], 'test: a::first')
        self.assertTrue(any('/logs' in url for _, url, _ in transport.calls))
        annotations[0]['message'] = 'error: generic process failure'
        for raw in (b'error: ' + b'x' * 4096, b'\xff',
                    '\n'.join(f'test case_{i} ... FAILED' for i in range(101)).encode()):
            routes[prefix + 'actions/jobs/403/logs'] = (200, raw)
            malformed = self.P.observe(self.options, opener=Transport(routes))
            self.assertFalse(malformed['complete'])
            self.assertIsNone(malformed['failures'][0]['normalized_fingerprint'])
        # Preserve the existing bounded annotation fallback if stronger logs are unavailable.
        annotations[0]['message'] = 'error: exact annotation fallback'
        routes[prefix + 'actions/jobs/403/logs'] = (404, b'not available')
        observed = self.P.observe(self.options, opener=Transport(routes))
        self.assertTrue(observed['complete'])
        self.assertEqual(observed['failures'][0]['normalized_fingerprint'], annotations[0]['message'])

    def test_partial_named_annotations_combine_with_logs_before_lexical_selection_and_bound_the_union(self):
        prefix = 'repos/octo/project/'
        signatures = []
        for lines in (['test z::last ... FAILED', 'test a::first ... FAILED'],
                      ['test a::first ... FAILED', 'test z::last ... FAILED']):
            routes = production_routes()
            annotations = [{'annotation_level': 'failure', 'message': 'test z::last ... FAILED'}]
            routes[prefix + 'check-runs/1403/annotations?per_page=100'] = annotations
            routes[prefix + 'commits/' + 'd' * 40 + '/check-runs?filter=latest&per_page=100']['check_runs'][0]['output']['annotations_count'] = 1
            routes[prefix + 'actions/jobs/403/logs'] = (200, ('\n'.join(lines) + '\n').encode())
            observed = self.P.observe(self.options, opener=Transport(routes))
            self.assertTrue(observed['complete'], observed['omissions'])
            failure, = observed['failures']
            self.assertEqual(failure['normalized_fingerprint'], 'test: a::first')
            self.assertEqual(failure['log_artifacts'][0], C.digest(annotations))
            self.assertEqual(len(failure['log_artifacts']), 2)
            signatures.append(failure['signature_id'])
        self.assertEqual(*signatures)
        # Neither source may hide the other, even when the annotation sorts first.
        annotations[0]['message'] = 'test 0::annotation ... FAILED'
        observed = self.P.observe(self.options, opener=Transport(routes))
        self.assertEqual(observed['failures'][0]['normalized_fingerprint'], 'test: 0::annotation')
        # Each source fits individually; their combined distinct identities must still fit.
        routes[prefix + 'actions/jobs/403/logs'] = (200, '\n'.join(f'test case_{i} ... FAILED' for i in range(99)).encode())
        exact = self.P.observe(self.options, opener=Transport(routes))
        self.assertTrue(exact['complete'], exact['omissions'])
        routes[prefix + 'actions/jobs/403/logs'] = (200, '\n'.join(f'test case_{i} ... FAILED' for i in range(100)).encode())
        excessive = self.P.observe(self.options, opener=Transport(routes))
        self.assertFalse(excessive['complete'])
        self.assertIsNone(excessive['failures'][0]['normalized_fingerprint'])
        # Named annotations survive a remotely unavailable log, but never malformed logs.
        routes[prefix + 'actions/jobs/403/logs'] = (404, b'unavailable')
        fallback = self.P.observe(self.options, opener=Transport(routes))
        self.assertTrue(fallback['complete'])
        self.assertEqual(fallback['failures'][0]['normalized_fingerprint'], 'test: 0::annotation')
        routes[prefix + 'actions/jobs/403/logs'] = (200, b'\xff')
        self.assertFalse(self.P.observe(self.options, opener=Transport(routes))['complete'])

    def test_production_unknown_missing_malformed_and_foreign_evidence_never_becomes_healthy(self):
        prefix = 'repos/octo/project/'
        job_route = prefix + 'actions/runs/301/attempts/2/jobs?per_page=100'
        check_route = prefix + 'commits/' + 'd' * 40 + '/check-runs?filter=latest&per_page=100'
        run_route = prefix + 'actions/runs?head_sha=' + 'd' * 40 + '&per_page=100'
        mutations = [
            lambda r: r[job_route]['jobs'][0].update(head_sha='a' * 40),
            lambda r: r[job_route]['jobs'][0].update(run_id=999),
            lambda r: r[job_route]['jobs'][0].update(run_attempt=1),
            lambda r: r[job_route]['jobs'][0].update(labels=[]),
            lambda r: r[job_route]['jobs'][0].update(name=None),
            lambda r: r[job_route]['jobs'][0].update(name='x' * 257),
            lambda r: r[job_route]['jobs'][0].update(check_run_url='https://evil.test/check-runs/1403'),
            lambda r: r[job_route]['jobs'][0].update(check_run_url=None),
            lambda r: r[job_route].update(total_count=101),
            lambda r: r[check_route]['check_runs'][0].update(head_sha='a' * 40),
            lambda r: r[check_route]['check_runs'][0].update(status='in_progress'),
            lambda r: r[check_route]['check_runs'][0].update(conclusion='success'),
            lambda r: r[run_route]['workflow_runs'][0].update(head_sha='a' * 40),
            lambda r: r[run_route]['workflow_runs'][0].update(run_attempt=None),
            lambda r: r[run_route]['workflow_runs'][0].update(path=None),
            lambda r: r[run_route]['workflow_runs'][0].update(status='in_progress'),
            lambda r: r[prefix + 'actions/runs/301/attempts/2'].update(head_sha='a' * 40),
            lambda r: r[prefix + 'actions/runs/301/attempts/2'].update(run_attempt=1),
            lambda r: r[prefix + 'actions/runs/301/attempts/2'].update(run_started_at=None),
            lambda r: r[prefix + 'actions/runs/301/attempts/2'].update(updated_at='2026-11-09T02:00:00Z'),
            lambda r: r[prefix + 'actions/runs/301/attempts/2'].update(updated_at='2026-11-09T05:00:00Z'),
            lambda r: r[prefix + 'pulls/7']['base']['repo'].update(full_name='foreign/repo'),
            lambda r: r.update({prefix + 'check-runs/1403/annotations?per_page=100': [{'annotation_level': 'failure', 'message': 7}]}),
            lambda r: r.update({prefix + 'check-runs/1403/annotations?per_page=100': []}) or r[check_route]['check_runs'][0]['output'].update(annotations_count=2),
            lambda r: r.update({prefix + 'actions/jobs/403/logs': (302, b'redirect refused')}),
            lambda r: r.update({prefix + 'actions/jobs/403/logs': (200, b'no deterministic failure evidence')}),
            lambda r: r.update({prefix + 'actions/jobs/403/logs': (200, b'\xff')}),
            lambda r: r.update({prefix + 'actions/jobs/403/logs': (200, b'error: ' + b'x' * 4096)}),
        ]
        for index, mutate in enumerate(mutations):
            with self.subTest(case=index):
                routes = production_routes()
                mutate(routes)
                snapshot = self.P.observe({**self.options, 'state': str(self.root / f'case-{index}.sqlite')}, opener=Transport(routes))
                self.assertFalse(snapshot['complete'], snapshot)
                self.assertTrue(snapshot['omissions'])
        # A line may fit the log-line cap and still exceed the fingerprint contract.
        # Keep the exact failed job as unknown, rather than losing its release alert.
        routes = production_routes()
        routes[prefix + 'actions/jobs/403/logs'] = (200, b'error: ' + b'x' * 2048)
        unknown = self.P.observe(self.options, opener=Transport(routes))
        self.assertFalse(unknown['complete'])
        self.assertEqual(unknown['failures'][0]['head_sha'], 'd' * 40)
        self.assertTrue(unknown['failures'][0]['release_blocking'])
        self.assertIsNone(unknown['failures'][0]['normalized_fingerprint'])
        for bad in [(404, b'secret server response'), (200, b'{bad'), (200, b'{"sha":"x","sha":"y"}'),
                    (200, b'[' * 1100 + b'0' + b']' * 1100)]:
            routes = production_routes()
            routes[prefix + 'commits/main'] = bad
            with self.assertRaises(ValueError):
                self.P.observe(self.options, opener=Transport(routes))

    def test_nightly_receipt_missing_old_foreign_malformed_and_unbounded_are_omissions(self):
        cases = [None, b'{', b'{"schema":"af.nightly-publish/0"}', b'x' * (self.P.RESPONSE_BYTES + 1)]
        for key, value in [('head_sha', 'd' * 40), ('repository', 'foreign/repo'), ('date', '2026-11-08'),
                           ('published_unix_ms', 0), ('published_unix_ms', C.SAFE), ('assets', []),
                           ('release', None), ('telegram_reason', 'both outcomes'), ('foreign', [self.publish['tag']])]:
            cases.append(C.canonical({**self.publish, key: value}))
        for index, raw in enumerate(cases):
            with self.subTest(case=index):
                if raw is None:
                    self.receipt.unlink()
                else:
                    self.receipt.write_bytes(raw)
                snapshot = self.P.observe(self.options, opener=Transport(production_routes()))
                self.assertIsNone(snapshot['nightly_receipt'])
                self.assertFalse(snapshot['complete'])
                self.assertTrue(any('nightly' in item for item in snapshot['omissions']))
        for reason, conclusion in [
            ('not sent: [publish] configures no telegram_token_file and telegram_chat_id', 'success'),
            ('Telegram sendMessage failed: HTTP 500', 'failure')]:
            self.receipt.write_bytes(C.canonical({**self.publish, 'telegram': None, 'telegram_reason': reason}))
            summary = self.P.receipt(self.receipt, 'octo/project', 'a' * 40,
                                    self.options['since'], self.options['captured_at'])
            self.assertEqual(summary['conclusion'], conclusion)
        self.receipt.unlink()
        self.receipt.symlink_to(self.root / 'missing')
        with self.assertRaises(ValueError):
            self.P.receipt(self.receipt, 'octo/project', 'a' * 40, self.options['since'], self.options['captured_at'])

    def test_production_transport_bounds_retries_errors_redirects_and_credentials(self):
        prefix = 'repos/octo/project/'
        route = prefix + 'commits/main'
        token = 'read_only_test_token'
        path = self.root / 'token'
        path.write_text(token)
        path.chmod(0o600)
        self.assertEqual(self.P.read_token(path), token)
        self.assertIsNone(self.P.read_token(None))
        for value in ('', token + '\nINJECT', 'x' * 4097, '\u2603'):
            path.write_text(value)
            with self.assertRaises(ValueError) as caught:
                self.P.read_token(path)
            self.assertNotIn(token, str(caught.exception))
        path.write_text(token)
        path.chmod(0o644)
        with self.assertRaisesRegex(ValueError, 'owner-only'):
            self.P.read_token(path)
        path.unlink()
        path.symlink_to(self.receipt)
        with self.assertRaises(ValueError):
            self.P.read_token(path)
        for bad in ((302, b'redirect'), (403, token.encode()), (200, C.canonical({'echo': token})),
                    (200, b'x' * (self.P.RESPONSE_BYTES + 1))):
            transport = Transport({route: bad}, token)
            client = self.P.Github('octo/project', token, opener=transport)
            with self.assertRaises(ValueError) as caught:
                client.get(route)
            self.assertNotIn(token, str(caught.exception))
            self.assertEqual(len(transport.calls), 1)
        for failure in ((503, b'server failure'), OSError(token)):
            transport = Transport({route: failure}, token)
            client = self.P.Github('octo/project', token, opener=transport)
            with self.assertRaises(ValueError) as caught:
                client.get(route)
            self.assertEqual(len(transport.calls), 2)
            self.assertNotIn(token, str(caught.exception))
        attempts = iter([(503, b''), (200, C.canonical({'sha': 'a' * 40}))])
        transport = Transport({route: lambda: next(attempts)})
        client = self.P.Github('octo/project', opener=transport, max_requests=2)
        self.assertEqual(client.get(route), {'sha': 'a' * 40})
        with self.assertRaisesRegex(ValueError, 'request bound'):
            client.get(route)
        self.assertEqual(len(transport.calls), 2)
        client = self.P.Github('octo/project', opener=Transport({route: (200, b'123456')}), max_bytes=10)
        self.assertEqual(client.read(route), b'123456')
        with self.assertRaisesRegex(ValueError, 'aggregate byte'):
            client.read(route)
        now = [0]
        client = self.P.Github('octo/project', opener=transport, clock=lambda: now[0])
        now[0] = 60
        with self.assertRaisesRegex(ValueError, 'wall bound'):
            client.read(route)
        self.assertEqual(client.requests, [])
        for invalid in ({'max_requests': 0}, {'max_requests': 33}, {'max_requests': True},
                        {'max_bytes': 0}, {'max_bytes': self.P.TOTAL_BYTES + 1},
                        {'max_wall_seconds': 0}, {'max_wall_seconds': 61}):
            with self.assertRaises(ValueError):
                self.P.Github('octo/project', opener=transport, **invalid)
        client = self.P.Github('octo/project', opener=transport)
        for escaped in ('https://evil.test/', 'repos/other/repo/commits/main', prefix + '../other', prefix + 'commits/main%0a'):
            with self.assertRaisesRegex(ValueError, 'escaped repository'):
                client.read(escaped)
        self.assertEqual(client.requests, [])
        # Exercise the actual nightly opener policy without making any request.
        opener = self.P.nightly().opener()
        redirect = next(handler for handler in opener.handlers if isinstance(handler, self.P.nightly().NoRedirect))
        self.assertIsNone(redirect.redirect_request(None, None, 302, '', {}, 'https://evil.test'))
        self.assertFalse(any(isinstance(h, urllib.request.ProxyHandler) and h.proxies for h in opener.handlers))
        self.assertEqual((self.P.nightly().CONNECT_SECONDS, self.P.nightly().READ_SECONDS), (5, 30))
        transport = Transport(production_routes())
        snapshot = self.P.observe({**self.options, 'max_requests': 5}, opener=transport)
        self.assertFalse(snapshot['complete'])
        self.assertEqual(len(transport.calls), 5)
        # Invoke the wall handler directly: no sleep or real-time deadline assertion.
        previous = self.P.signal.getsignal(self.P.signal.SIGALRM)
        with self.assertRaisesRegex(ValueError, 'wall bound'):
            with self.P.wall_bound(60):
                self.P.signal.getsignal(self.P.signal.SIGALRM)(self.P.signal.SIGALRM, None)
        self.assertIs(self.P.signal.getsignal(self.P.signal.SIGALRM), previous)
        self.assertEqual(self.P.signal.getitimer(self.P.signal.ITIMER_REAL), (0.0, 0.0))

    def test_read_only_adapter_emits_exact_snapshot_refuses_effects_and_checks_revision_before_state(self):
        request = self.request()
        response = self.P.adapter_response(request, self.staged_options(), opener=Transport(production_routes()))
        self.assertEqual(response['outcome'], 'done')
        self.assertEqual((response['intent_id'], response['input_digest']), (request['intent_id'], request['input_digest']))
        evidence = C.loads(response['observation']['evidence'])
        C.validate_snapshot(evidence)
        self.assertEqual(response['evidence'], 'ci_observe_v1 snapshot ' + C.digest(evidence))
        self.assertEqual(evidence['failures'][0]['head_sha'], 'd' * 40)
        self.assertLessEqual(len(C.canonical(response)), 65536)
        before = (self.root / 'observer.sqlite').read_bytes()
        request['inputs']['revision']['value'] = 'b' * 40
        failed = self.P.adapter_response(request, self.staged_options(), opener=Transport(production_routes()))
        self.assertEqual((failed['outcome'], failed['observation']), ('failed', None))
        self.assertEqual((self.root / 'observer.sqlite').read_bytes(), before)
        for changes in ({'operation': 'rerun', 'method': 'execute'}, {'operation': 'notify'}, {'destination': 'github:foreign/repo'}):
            transport = Transport({})
            with self.assertRaises(ValueError):
                self.P.adapter_response(self.request(**changes), self.staged_options(), opener=transport)
            self.assertEqual(transport.calls, [])

    def test_installed_adapter_bounds_escaped_unicode_response_before_capture_and_retains_findings(self):
        for fingerprint in ('核"\\' * 675, '"\\' * 1000):
            with self.subTest(fingerprint=fingerprint[:6]), tempfile.TemporaryDirectory() as directory:
                options = {**self.options, 'max_requests': 32, 'state': str(Path(directory) / 'observer.sqlite')}
                routes = near_limit_routes(fingerprint)
                collected = self.P.collect(self.P.Github('octo/project', max_requests=32, opener=Transport(routes)),
                    captured_at=options['captured_at'], main_ref=options['main_ref'], since=options['since'],
                    nightly_receipt=options['nightly_receipt'], release_pr=options['release_pr'])
                with closing(C.Journal(Path(directory) / 'unbounded.sqlite')) as journal:
                    unbounded = C.observe(collected, journal)
                self.assertTrue(unbounded['complete'], unbounded['omissions'])
                self.assertGreater(len(C.canonical(unbounded)), C.MAX_BYTES // 2)
                self.assertLessEqual(len(C.canonical(unbounded)), C.MAX_BYTES)
                self.assertGreater(len(C.canonical(self.P.observation_response(unbounded))), C.MAX_BYTES)
                snapshot = self.P.observe(options, opener=Transport(routes))
                self.assertFalse(snapshot['complete'])
                self.assertIn('64 KiB adapter response bound exceeded', snapshot['omissions'])
                self.assertTrue(snapshot['failures'])
                self.assertLess(len(snapshot['failures']), len(unbounded['failures']))
                self.install_occurrence(snapshot)
                staged = {**self.staged_options(), 'state': options['state'], 'max_requests': 32}
                executable = Path(directory) / 'adapter'
                executable.write_bytes(self.P.adapter_bytes(staged))
                request = self.request_without_install(snapshot)
                before = Path(options['state']).read_bytes()
                for _ in range(2):
                    response = self.run_staged(executable, request, Transport(routes))
                    self.assertEqual(response['outcome'], 'done', response)
                    self.assertTrue(response['observation']['satisfied'])
                    self.assertEqual(C.loads(response['observation']['evidence']), snapshot)
                    self.assertLessEqual(len(C.canonical(response)) + 1, C.MAX_BYTES)
                    self.assertEqual(Path(options['state']).read_bytes(), before)
                with closing(C.Journal(options['state'])) as journal:
                    self.assertEqual(journal.metadata()['capture']['snapshot'], snapshot)
                spec = importlib.util.spec_from_file_location('ci_scenario', FIXTURE / 'configure.py')
                compiler = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(compiler)
                target = {'repo': '/fixture/repo', 'state': '/fixture/state', 'task_id': 'diagnose',
                    'plan_id': C.digest('plan'), 'revision_id': C.digest('revision'), 'max_tokens': 24096}
                steps = compiler.bundle(snapshot, C.digest(snapshot), target)['definitions']['maintenance-ci-health']['steps']
                findings = [C.loads(s['inputs']['body']['value']['value']) for s in steps if s['id'].startswith('notify-findings')]
                self.assertEqual([f for message in findings for f in message['failures']], C.failure_summaries(snapshot))
                self.assertEqual([o for message in findings for o in message['omissions']], snapshot['omissions'])
                self.assertFalse(any(s['id'] == 'diagnose' for s in steps))
                # Exercise the installed executable and notification service as well as
                # the compiler. Native coverage consumes this same wire in a Scenario.
                notifications = Path(directory) / 'service.json'
                notifications.write_bytes(C.canonical({'snapshot': snapshot, 'snapshot_id': C.digest(snapshot),
                    'scenario_run': 'ci-health-fixture'}))
                executable.write_bytes(hermetic_production_adapter(self.P, staged, routes, compiler.adapter_bytes(ROOT)))
                executable.chmod(0o700)
                observed = subprocess.run([str(executable)], input=C.canonical(request), capture_output=True, check=True)
                self.assertLessEqual(len(observed.stdout), C.MAX_BYTES)
                response = C.loads(observed.stdout)
                self.assertEqual(response['outcome'], 'done', response)
                self.assertEqual(C.loads(response['observation']['evidence']), snapshot)
                for step in (s for s in steps if s['id'].startswith('notify-findings')):
                    inputs = {'subject': {'type': 'text', 'value': C.digest(snapshot['repository'])},
                        'revision': {'type': 'text', 'value': snapshot['source_sha']},
                        'body': step['inputs']['body']['value']}
                    notify = {'schema': 'af/scenario-adapter-request@4', 'method': 'execute', 'operation': 'notify',
                        'destination': 'fixture:ci-health', 'intent_id': C.digest(step['id']),
                        'input_digest': C.digest(inputs), 'inputs': inputs}
                    reported = subprocess.run([str(executable)], input=C.canonical(notify), capture_output=True, check=True)
                    self.assertEqual(C.loads(reported.stdout)['outcome'], 'done')
                events = [C.loads(line) for line in (Path(directory) / 'events').read_bytes().splitlines()]
                self.assertEqual([f for message in events for f in message['failures']], C.failure_summaries(snapshot))
                self.assertEqual([o for message in events for o in message['omissions']], snapshot['omissions'])
                self.assertEqual([a for message in events for a in message['release_alerts']],
                    [f['occurrence_id'] for f in snapshot['failures'] if f['release_blocking']])

        raw = service()
        raw['jobs'] = []
        raw['nightly_receipt']['conclusion'] = 'success'
        raw['omissions'] = ['"\\' * 100 + str(i) for i in range(100)]
        with closing(C.Journal(self.root / 'omissions.sqlite')) as journal:
            snapshot = C.observe(raw, journal,
                snapshot_fits=lambda value: len(C.canonical(self.P.observation_response(value))) + 1 <= C.MAX_BYTES)
            self.assertFalse(snapshot['complete'])
            self.assertIn('64 KiB adapter response bound exceeded', snapshot['omissions'])
            self.assertLess(len(snapshot['omissions']), 100)
            self.assertLessEqual(len(C.canonical(self.P.observation_response(snapshot))) + 1, C.MAX_BYTES)
            before = journal.metadata()
            rows = journal.db.execute('SELECT * FROM signatures').fetchall()
            with self.assertRaises(ValueError):
                C.observe(service(), journal, snapshot_fits=lambda _value: False)
            self.assertEqual(journal.metadata(), before)
            self.assertEqual(journal.db.execute('SELECT * FROM signatures').fetchall(), rows)

    def run_staged(self, executable, request, transport):
        output = io.StringIO()
        with patch.dict(sys.modules), patch.object(sys, 'stdin', io.TextIOWrapper(io.BytesIO(C.canonical(request)))), \
             patch.object(urllib.request.OpenerDirector, 'open', lambda _opener, req, timeout: transport.open(req, timeout)), \
             redirect_stdout(output):
            runpy.run_path(str(executable), run_name='__main__')
        return C.loads(output.getvalue())

    def test_staged_adapter_replays_captured_recurring_attempts_and_refuses_changes_without_journal_mutation(self):
        routes = production_routes()
        prefix = 'repos/octo/project/'
        checks = routes[prefix + 'commits/' + 'd' * 40 + '/check-runs?filter=latest&per_page=100']
        checks['check_runs'].append({**copy.deepcopy(checks['check_runs'][0]), 'id': 1404})
        checks['total_count'] = 2
        runs = routes[prefix + 'actions/runs?head_sha=' + 'd' * 40 + '&per_page=100']
        runs['workflow_runs'].append({**runs['workflow_runs'][0], 'id': 302})
        runs['total_count'] = 2
        routes[prefix + 'actions/runs/302/attempts/2'] = {
            **routes[prefix + 'actions/runs/301/attempts/2'], 'id': 302}
        job = routes[prefix + 'actions/runs/301/attempts/2/jobs?per_page=100']['jobs'][0]
        routes[prefix + 'actions/runs/302/attempts/2/jobs?per_page=100'] = {
            'total_count': 1, 'jobs': [{**job, 'id': 404, 'run_id': 302,
                'check_run_url': 'https://api.github.com/' + prefix + 'check-runs/1404'}]}
        routes[prefix + 'check-runs/1404/annotations?per_page=100'] = []
        routes[prefix + 'actions/jobs/404/logs'] = routes[prefix + 'actions/jobs/403/logs']
        # Capture and stage against the very same persistent journal the installed adapter uses.
        snapshot = self.P.observe(self.options, opener=Transport(routes))
        self.assertTrue(snapshot['complete'], snapshot['omissions'])
        self.assertEqual(len({f['signature_id'] for f in snapshot['failures']}), 1)
        self.assertEqual({f['run_id']: f['occurrence_count'] for f in snapshot['failures']}, {301: 1, 302: 2})
        self.P.stage_occurrence(snapshot, since=self.options['since'], nightly_receipt=self.receipt,
                                occurrence_dir=self.staged_options()['occurrence_dir'])
        request = self.request_without_install(snapshot)
        executable = self.root / 'adapter'
        executable.write_bytes(self.P.adapter_bytes(self.staged_options()))
        before = Path(self.options['state']).read_bytes()
        for mutation in ('log', 'green'):
            changed = copy.deepcopy(routes)
            if mutation == 'log':
                changed[prefix + 'actions/jobs/403/logs'] = (200, b'error: changed assertion\n')
            else:
                changed[prefix + 'actions/runs/301/attempts/2/jobs?per_page=100']['jobs'][0]['conclusion'] = 'success'
                changed[prefix + 'commits/' + 'd' * 40 + '/check-runs?filter=latest&per_page=100']['check_runs'][0]['conclusion'] = 'success'
            with self.subTest(mutation=mutation):
                response = self.run_staged(executable, request, Transport(changed))
                self.assertEqual((response['outcome'], response['observation']), ('failed', None))
                self.assertEqual(Path(self.options['state']).read_bytes(), before)
        for _ in range(2):
            response = self.run_staged(executable, request, Transport(routes))
            self.assertEqual(response['outcome'], 'done', response)
            self.assertEqual(C.loads(response['observation']['evidence']), snapshot)
            self.assertEqual(Path(self.options['state']).read_bytes(), before)
        runs['workflow_runs'].reverse()
        checks['check_runs'].reverse()
        reordered = self.run_staged(executable, request, Transport(routes))
        self.assertEqual(reordered['outcome'], 'done', reordered)
        self.assertEqual(C.loads(reordered['observation']['evidence']), snapshot)
        self.assertEqual(Path(self.options['state']).read_bytes(), before)

    def test_same_main_changed_job_evidence_refuses_scenario_snapshot_before_notifications(self):
        snapshot, _ = self.install_occurrence()
        spec = importlib.util.spec_from_file_location('ci_scenario_binding', FIXTURE / 'configure.py')
        compiler = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(compiler)
        target = {'repo': '/fixture/repo', 'state': '/fixture/state', 'task_id': 'diagnose',
                  'plan_id': C.digest('plan'), 'revision_id': C.digest('revision'), 'max_tokens': 24096}
        bundle = compiler.bundle(snapshot, C.digest(snapshot), target)
        steps = bundle['definitions'][bundle['root']]['steps']
        self.assertEqual(steps[0]['id'], 'observe')
        self.assertEqual(steps[1]['id'], 'notify-findings')
        self.assertTrue(any(step['id'] == 'diagnose' for step in steps))
        request = self.request()
        request['inputs']['condition']['value'] = steps[0]['operation']['condition']
        executable = self.root / 'adapter'
        executable.write_bytes(self.P.adapter_bytes(self.staged_options()))
        accepted = self.run_staged(executable, request, Transport(production_routes()))
        self.assertEqual(accepted['outcome'], 'done')
        before = (self.root / 'observer.sqlite').read_bytes()
        for mutation in ('log', 'attempt', 'green', 'main-job'):
            routes = production_routes()
            if mutation == 'log':
                routes['repos/octo/project/actions/jobs/403/logs'] = (200, b'error: changed assertion\n')
            elif mutation == 'attempt':
                run = routes['repos/octo/project/actions/runs?head_sha=' + 'd' * 40 + '&per_page=100']['workflow_runs'][0]
                run['run_attempt'] = 3  # Missing exact attempt makes the observed snapshot incomplete.
            elif mutation == 'green':
                routes['repos/octo/project/actions/runs/301/attempts/2/jobs?per_page=100']['jobs'][0]['conclusion'] = 'success'
                routes['repos/octo/project/commits/' + 'd' * 40 + '/check-runs?filter=latest&per_page=100']['check_runs'][0]['conclusion'] = 'success'
            else:
                # Green jobs are absent from snapshot.failures but remain capture evidence.
                routes['repos/octo/project/actions/runs/101/attempts/2/jobs?per_page=100']['jobs'][0]['name'] = 'other'
                routes['repos/octo/project/commits/' + 'a' * 40 + '/check-runs?filter=latest&per_page=100']['check_runs'][0]['name'] = 'other'
            with self.subTest(mutation=mutation):
                failed = self.run_staged(executable, request, Transport(routes))
                self.assertEqual((failed['outcome'], failed['observation']), ('failed', None))
                self.assertEqual((self.root / 'observer.sqlite').read_bytes(), before)

    def test_one_staged_adapter_observes_two_owner_authorized_daily_windows_and_receipts(self):
        first = self.request()
        executable = self.root / 'adapter'
        executable.write_bytes(self.P.adapter_bytes(self.staged_options()))
        pinned = executable.read_bytes()
        observed = self.run_staged(executable, first, Transport(production_routes()))
        self.assertEqual(observed['outcome'], 'done')
        first_snapshot = C.loads(observed['observation']['evidence'])
        tomorrow = copy.deepcopy(self.publish)
        tomorrow.update(date='2026-11-10', tag='nightly-2026.11.10',
                        published_unix_ms=int(C.instant('2026-11-10T03:30:00Z').timestamp() * 1000))
        tomorrow['release']['url'] = tomorrow['release']['url'].replace('2026.11.09', '2026.11.10')
        for asset in tomorrow['assets']:
            asset['name'] = asset['name'].replace('2026.11.09', '2026.11.10')
        receipt = self.root / 'tomorrow-publish.json'
        receipt.write_bytes(C.canonical(tomorrow))
        routes = production_routes()
        for run_id in (101, 301):
            routes[f'repos/octo/project/actions/runs/{run_id}/attempts/2'].update(
                run_started_at='2026-11-10T03:00:00Z', updated_at='2026-11-10T03:01:00Z')
        for job_id in (201, 403):
            for run_id in (101, 301):
                for job in routes[f'repos/octo/project/actions/runs/{run_id}/attempts/2/jobs?per_page=100']['jobs']:
                    if job['id'] == job_id:
                        job['completed_at'] = '2026-11-10T03:01:00Z'
        # Use another owner-selected release PR, still bound to the pinned repository.
        routes['repos/octo/project/pulls/8'] = {**routes['repos/octo/project/pulls/7'], 'number': 8}
        options = {**self.options, 'since': '2026-11-09T04:00:00Z', 'captured_at': '2026-11-10T04:00:00Z',
                   'nightly_receipt': str(receipt), 'release_pr': 8}
        expected = self.P.observe(options, opener=Transport(routes))
        self.install_occurrence(expected, since=options['since'], nightly_receipt=str(receipt), release_pr=8)
        second = copy.deepcopy(first)
        second['inputs']['condition']['value'] = 'ci_observe_v1:' + C.digest(expected)
        second['intent_id'] = C.digest('tomorrow')
        second['input_digest'] = C.digest(second['inputs'])
        observed = self.run_staged(executable, second, Transport(routes))
        self.assertEqual(observed['outcome'], 'done', observed)
        second_snapshot = C.loads(observed['observation']['evidence'])
        self.assertEqual(second_snapshot, expected)
        self.assertTrue(second_snapshot['complete'], second_snapshot['omissions'])
        self.assertNotEqual(first_snapshot['captured_at'], second_snapshot['captured_at'])
        self.assertNotEqual(first_snapshot['nightly_receipt']['identity'], second_snapshot['nightly_receipt']['identity'])
        self.assertEqual(second_snapshot['release_pr']['number'], 8)
        self.assertEqual(executable.read_bytes(), pinned)
        self.assertNotIn(str(receipt).encode(), pinned)
        for key in ('since', 'captured_at', 'nightly_receipt', 'release_pr'):
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, 'freeze'):
                self.P.adapter_bytes({**self.staged_options(), key: self.options[key]})

    def test_occurrence_schema_writer_reader_parity_and_unauthorized_selectors_fail_closed(self):
        snapshot, path = self.install_occurrence()
        original = path.read_bytes()
        document = C.loads(original)
        schema = C.loads((ROOT / 'schemas/ci-observe-occurrence-v1.json').read_bytes())
        validator = self.P.module('occurrence_validator', ROOT / 'scripts/backlog-check-triage.py').Validator()
        validator.schemas[schema['$id']] = schema
        self.assertEqual(validator.errors(document, schema['$id']), [])
        self.assertEqual(self.P.occurrence_options(self.staged_options(), C.digest(snapshot))['since'], self.options['since'])
        for changes in ({'schema': 'af.ci-observe-occurrence/0'}, {'extra': True}, {'release_pr': True},
                        {'snapshot_digest': C.digest('foreign')}, {'since': '2026-11-07T04:00:00Z'},
                        {'since': document['captured_at']}, {'nightly_receipt': '../secret'}):
            path.write_bytes(C.canonical({**document, **changes}))
            transport = Transport({})
            response = self.P.adapter_response(self.request_without_install(snapshot), self.staged_options(), opener=transport)
            self.assertEqual((response['outcome'], response['observation']), ('failed', None))
            self.assertEqual(transport.calls, [])
        for mode in (0o644, 0o660):
            path.write_bytes(original)
            path.chmod(mode)
            with self.assertRaises(ValueError):
                self.P.occurrence_options(self.staged_options(), C.digest(snapshot))
        path.chmod(0o600)
        path.write_bytes(b'x' * 8193)
        with self.assertRaises(ValueError):
            self.P.occurrence_options(self.staged_options(), C.digest(snapshot))
        path.unlink()
        target = self.root / 'selectors.json'
        target.write_bytes(original)
        target.chmod(0o600)
        path.symlink_to(target)
        with self.assertRaises(OSError):
            self.P.occurrence_options(self.staged_options(), C.digest(snapshot))
        path.unlink()
        path.mkdir()
        with self.assertRaises(ValueError):
            self.P.occurrence_options(self.staged_options(), C.digest(snapshot))
        path.rmdir()
        Path(self.staged_options()['occurrence_dir']).chmod(0o755)
        with self.assertRaises(ValueError):
            self.P.occurrence_options(self.staged_options(), C.digest(snapshot))

    def request_without_install(self, snapshot):
        inputs = {key: {'type': 'text', 'value': value} for key, value in {
            'subject': 'octo/project', 'revision': 'a' * 40,
            'condition': 'ci_observe_v1:' + C.digest(snapshot)}.items()}
        return {'schema': 'af/scenario-adapter-request@4', 'method': 'observe', 'operation': 'observe',
                'destination': self.options['destination'], 'intent_id': C.digest('observe'),
                'input_digest': C.digest(inputs), 'inputs': inputs}

    def test_stage_occurrence_cli_writes_only_absent_owner_selectors_without_observing(self):
        snapshot, path = self.install_occurrence()
        path.unlink()
        selected = self.root / 'selected.json'
        selected.write_bytes(C.canonical(snapshot))
        command = [sys.executable, '-I', str(ROOT / 'scripts/ci-health.py'), 'stage-occurrence',
                   '--snapshot', str(selected), '--since', self.options['since'],
                   '--nightly-receipt', str(self.receipt), '--occurrence-dir', str(path.parent)]
        result = subprocess.run(command, capture_output=True, check=True)
        self.assertEqual(result.stdout, b'')
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(C.loads(path.read_bytes()), C.occurrence_document(snapshot,
            since=self.options['since'], nightly_receipt=self.receipt))
        self.assertFalse(Path(self.options['state']).exists())
        before = path.read_bytes()
        self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)
        self.assertEqual(path.read_bytes(), before)
        for flag, value in (('--since', '2026-11-01T04:00:00Z'), ('--nightly-receipt', '../foreign')):
            path.unlink()
            invalid = command.copy()
            invalid[invalid.index(flag) + 1] = value
            self.assertNotEqual(subprocess.run(invalid, capture_output=True).returncode, 0)
            self.assertFalse(path.exists())
            path.write_bytes(before)
        alias = self.root / 'occurrences-alias'
        alias.symlink_to(path.parent, target_is_directory=True)
        with self.assertRaises(OSError):
            self.P.occurrence_options({**self.staged_options(), 'occurrence_dir': str(alias)}, C.digest(snapshot))

    def test_signed_job_log_redirect_is_complete_without_forwarding_credentials(self):
        routes = production_routes()
        url = 'https://productionresultssa0.blob.core.windows.net/actions-results/job/logs?sv=2025-01-05&se=2026-11-09T04%3A01%3A00Z&sr=b&sp=r&spr=https&sig=SIGNED'
        route = 'repos/octo/project/actions/jobs/403/logs'
        raw = b'error: generic harness error\ntest z::second ... FAILED\ntest a::first ... FAILED\n'
        routes[route] = (302, b'', {'Location': url})
        routes[url] = (200, raw)
        token = self.root / 'token'
        token.write_text('github_pat_test_only')
        token.chmod(0o600)
        transport = Transport(routes, token='github_pat_test_only')
        snapshot = self.P.observe({**self.options, 'github_token_file': str(token)}, opener=transport)
        self.assertTrue(snapshot['complete'], snapshot['omissions'])
        self.assertEqual(snapshot['failures'][0]['normalized_fingerprint'], 'test: a::first')
        self.assertEqual(snapshot['failures'][0]['log_artifacts'], ['sha256:' + __import__('hashlib').sha256(raw).hexdigest()])
        self.assertEqual(len(transport.calls), 13)
        self.assertTrue(all(response.closed for response in transport.responses))
        # A real urllib no-redirect opener raises HTTPError for the API's 302.
        routes[route] = urllib.error.HTTPError('https://api.github.com/' + route, 302, 'Found', {'Location': url}, io.BytesIO(b''))
        self.assertTrue(self.P.observe(self.options, opener=Transport(routes))['complete'])

    def test_signed_log_download_refuses_foreign_locations_chains_and_preserves_all_bounds(self):
        route = 'repos/octo/project/actions/jobs/403/logs'
        url = 'https://productionresultssa0.blob.core.windows.net/log?sv=1&se=2026-11-09&sr=b&sp=r&sig=SIGNED'
        invalid = ['http:' + url[6:], url.replace('productionresultssa0', 'evil'),
                   url.replace('.net/', '.net.evil.test/'), url.replace('https://', 'https://user@'),
                   url.replace('.net/', '.net:443/'), url + '#fragment', url.replace('sp=r', 'sp=rw'),
                   url.replace('&sig=SIGNED', ''), url + '&sig=other', url.replace('/log?', '/../log?'),
                   url.replace('/log?', '/%2e%2e/log?'), url + '\n', '/relative', url.replace('/log?', '/log\\evil?')]
        for location in invalid:
            transport = Transport({route: (302, b'', {'Location': location})})
            client = self.P.Github('octo/project', opener=transport)
            with self.subTest(location=location), self.assertRaises(ValueError):
                client.read(route, log=True)
            self.assertEqual(len(transport.calls), 1)
        for answer in ((302, b'', {'Location': url}), (200, b'x' * (self.P.RESPONSE_BYTES + 1)),
                       (200, b'github_pat_test_only')):
            transport = Transport({route: (302, b'', {'Location': url}), url: answer}, token='github_pat_test_only')
            client = self.P.Github('octo/project', 'github_pat_test_only', opener=transport)
            with self.assertRaises(ValueError):
                client.read(route, log=True)
            self.assertEqual(len(transport.calls), 2)
        for bound in ({'max_requests': 1}, {'max_bytes': 1}):
            transport = Transport({route: (302, b'x', {'Location': url}), url: (200, b'error')})
            with self.assertRaises(ValueError):
                self.P.Github('octo/project', opener=transport, **bound).read(route, log=True)
            self.assertEqual(len(transport.calls), 1)
        now = [0]
        def redirect():
            now[0] = 60
            return 302, b'', {'Location': url}
        transport = Transport({route: redirect, url: (200, b'error')})
        with self.assertRaisesRegex(ValueError, 'wall bound'):
            self.P.Github('octo/project', opener=transport, clock=lambda: now[0]).read(route, log=True)
        self.assertEqual(len(transport.calls), 1)
        answers = iter([(503, b''), (200, b'error: downloaded')])
        transport = Transport({route: (302, b'', {'Location': url}), url: lambda: next(answers)})
        client = self.P.Github('octo/project', opener=transport, max_requests=3)
        self.assertEqual(client.read(route, log=True), b'error: downloaded')
        self.assertEqual(len(client.requests), 3)
        self.assertNotIn('SIGNED', repr(client.requests))

    def test_production_logs_select_stable_lexical_test_identity_before_generic_errors(self):
        route = 'repos/octo/project/actions/jobs/403/logs'
        families = [('test z::last ... FAILED', 'test a::first ... FAILED', 'a::first'),
                    ('FAIL [   0.100s] (   2/2) suite z::last', 'FAIL [ 0.200s] ( 1/2) suite a::first', 'suite a::first'),
                    ('FAILED tests/test_x.py::test_z - assertion', 'FAILED tests/test_x.py::test_a - assertion', 'tests/test_x.py::test_a'),
                    ('FAIL: test_z (suite.Tests.test_z)', 'FAIL: test_a (suite.Tests.test_a)', 'test_a (suite.Tests.test_a)')]
        for last, first, expected in families:
            signatures = []
            for index, lines in enumerate(([last, first], [first, last], [last])):
                routes = production_routes()
                routes[route] = (200, ('error: generic changing harness error ' + str(index) + '\n' + '\n'.join(lines) + '\n').encode())
                snapshot = self.P.observe(self.options, opener=Transport(routes))
                self.assertTrue(snapshot['complete'], snapshot['omissions'])
                failure, = snapshot['failures']
                signatures.append(failure['signature_id'])
                if index < 2:
                    self.assertEqual(failure['normalized_fingerprint'], 'test: ' + expected)
            self.assertEqual(signatures[0], signatures[1])
            self.assertNotEqual(signatures[0], signatures[2])
        for lines in ([f'test case_{i} ... FAILED' for i in range(101)], ['test ' + 'x' * 2043 + ' ... FAILED']):
            routes = production_routes()
            routes[route] = (200, ('error: generic\n' + '\n'.join(lines)).encode())
            snapshot = self.P.observe(self.options, opener=Transport(routes))
            self.assertFalse(snapshot['complete'])
            self.assertIsNone(snapshot['failures'][0]['normalized_fingerprint'])

    def test_failing_test_parameters_preserve_shas_durations_and_paths_in_distinct_identities(self):
        route = 'repos/octo/project/actions/jobs/403/logs'
        signatures = []
        for parameter in ('a' * 40, 'b' * 40, '10ms', '20ms', '/tmp/first', '/tmp/second', 'run 123', 'run 456', 'a - b', 'a - c', 'nested[a - b]'):
            identity = 'tests/test_x.py::test_value[' + parameter + ']'
            routes = production_routes()
            routes[route] = (200, ('error: generic\n2026-11-09T03:01:00Z \x1b[31mFAILED ' + identity + ' - AssertionError\x1b[0m\n').encode())
            snapshot = self.P.observe(self.options, opener=Transport(routes))
            self.assertTrue(snapshot['complete'], snapshot['omissions'])
            failure, = snapshot['failures']
            self.assertEqual(failure['normalized_fingerprint'], 'test: ' + identity)
            signatures.append(failure['signature_id'])
        self.assertEqual(len(set(signatures)), len(signatures))

    def test_staged_adapter_is_self_contained_pins_helpers_and_staging_never_reads_secrets_or_network(self):
        executable = self.root / 'installed-observer'
        flags = []
        for key, value in self.staged_options().items():
            if value is not None:
                flags += ['--' + key.replace('_', '-'), str(value)]
        command = [sys.executable, '-I', str(ROOT / 'scripts/ci-health.py'), 'stage-adapter', *flags,
                   '--output', str(executable)]
        subprocess.run(command, capture_output=True, check=True)
        self.assertFalse((self.root / 'observer.sqlite').exists())
        program = executable.read_bytes()
        self.assertEqual(program, self.P.adapter_bytes(self.staged_options()))
        self.assertEqual(executable.stat().st_mode & 0o777, 0o700)
        again = subprocess.run(command, capture_output=True)
        self.assertNotEqual(again.returncode, 0)
        self.assertEqual(executable.read_bytes(), program)
        transport = Transport(production_routes())
        output = io.StringIO()
        # Only the opener is replaced: run the staged entry point, bundled nightly transport,
        # receipt schema and native protocol with no source/helper imports from its directory.
        with patch.dict(sys.modules), patch.object(sys, 'stdin', io.TextIOWrapper(io.BytesIO(C.canonical(self.request())))), \
             patch.object(urllib.request.OpenerDirector, 'open', lambda _opener, req, timeout: transport.open(req, timeout)), \
             redirect_stdout(output):
            runpy.run_path(str(executable), run_name='__main__')
        response = C.loads(output.getvalue())
        self.assertEqual(response['outcome'], 'done', response)
        self.assertTrue(C.loads(response['observation']['evidence'])['complete'])
        self.assertEqual(len(transport.calls), 12)
        # A token path is staged verbatim without reading it or copying any secret bytes.
        missing = {**self.staged_options(), 'github_token_file': str(self.root / 'not-installed-credential')}
        self.assertIn(missing['github_token_file'].encode(), self.P.adapter_bytes(missing))

    def test_green_closure_uses_attempt_completion_order_not_run_ids(self):
        state = str(self.root / 'temporal.sqlite')
        routes = production_routes()
        prefix = 'repos/octo/project/'
        # The lower-ID rerun fails later. A higher-ID success completed earlier must not close it.
        selected = prefix + 'actions/runs/301/attempts/2'
        routes[selected]['updated_at'] = '2026-11-09T03:31:00Z'
        add_production_job(routes, 'd' * 40, 302, 1, 404, 'success',
                           '2026-11-09T03:00:00Z', '2026-11-09T03:01:00Z',
                           labels=['macos-14', 'ARM64'])
        routes[prefix + 'actions/runs?head_sha=' + 'd' * 40 + '&per_page=100']['workflow_runs'].reverse()
        snapshot = self.P.observe({**self.options, 'state': state}, opener=Transport(routes))
        failure, = snapshot['failures']
        self.assertEqual((failure['run_id'], failure['run_attempt'], failure['job_id']), (301, 2, 403))
        self.assertTrue(failure['release_blocking'])

        # Re-observing only the older success cannot erase the retained failure or recurrence.
        successful = production_routes()
        for key in list(successful):
            if any(part in key for part in ('/301/', '/302/', 'jobs/403', 'jobs/404')):
                successful.pop(key)
        runs_key = prefix + 'actions/runs?head_sha=' + 'd' * 40 + '&per_page=100'
        successful[runs_key]['workflow_runs'] = [run for run in successful[runs_key]['workflow_runs'] if run['id'] == 302]
        checks_key = prefix + 'commits/' + 'd' * 40 + '/check-runs?filter=latest&per_page=100'
        successful[checks_key]['check_runs'] = [check for check in successful[checks_key]['check_runs'] if check['id'] == 1404]
        successful[checks_key]['total_count'] = 1
        add_production_job(successful, 'd' * 40, 302, 1, 404, 'success',
                           '2026-11-09T03:00:00Z', '2026-11-09T03:01:00Z',
                           labels=['macos-14', 'ARM64'])
        # Remove the duplicate triple inserted by the convenience initializer above.
        successful[runs_key]['workflow_runs'] = successful[runs_key]['workflow_runs'][:1]
        successful[prefix + 'actions/runs/302/attempts/1/jobs?per_page=100']['jobs'] = successful[prefix + 'actions/runs/302/attempts/1/jobs?per_page=100']['jobs'][:1]
        successful[prefix + 'actions/runs/302/attempts/1/jobs?per_page=100']['total_count'] = 1
        successful[checks_key]['check_runs'] = successful[checks_key]['check_runs'][:1]
        snapshot = self.P.observe({**self.options, 'state': state}, opener=Transport(successful))
        self.assertEqual([f['job_id'] for f in snapshot['failures']], [403])
        self.assertFalse(snapshot['complete'])
        self.assertTrue(any('green closure' in reason for reason in snapshot['omissions']))

    def test_malformed_or_future_completion_cannot_abort_or_close_release_failure(self):
        prefix = 'repos/octo/project/'
        for invalid in ('not-a-time', '2099-01-01T00:00:00Z'):
            routes = production_routes()
            jobs_key = prefix + 'actions/runs/302/attempts/1/jobs?per_page=100'
            add_production_job(routes, 'd' * 40, 302, 1, 404, 'success',
                               '2026-11-09T03:00:00Z', '2026-11-09T03:30:00Z',
                               labels=['macos-14', 'ARM64'])
            routes[jobs_key]['jobs'][0]['completed_at'] = invalid
            snapshot = self.P.observe(self.options, opener=Transport(routes))
            self.assertFalse(snapshot['complete'], invalid)
            failure, = snapshot['failures']
            self.assertEqual(failure['job_id'], 403, invalid)
            self.assertTrue(failure['release_blocking'])
        routes = production_routes()
        routes[prefix + 'actions/runs/101/attempts/2/jobs?per_page=100']['jobs'][0]['completed_at'] = 'invalid'
        routes[prefix + 'actions/runs?head_sha=' + 'd' * 40 + '&per_page=100']['workflow_runs'].reverse()
        snapshot = self.P.observe(self.options, opener=Transport(routes))
        self.assertFalse(snapshot['complete'])
        self.assertEqual(snapshot['failures'][0]['job_id'], 403)

    def test_unfinished_attempt_metadata_never_suppresses_failed_siblings(self):
        prefix = 'repos/octo/project/'
        for position in ('first', 'last'):
            for mode in ('null_start', 'unavailable_attempt'):
                routes = production_routes()
                queued = {'id': 1998, 'workflow_id': 1, 'run_attempt': 1,
                    'head_sha': 'd' * 40, 'status': 'in_progress',
                    'path': '.github/workflows/validate.yml', 'created_at': '2026-11-09T03:20:00Z',
                    'repository': {'full_name': 'octo/project'}}
                runs_key = prefix + 'actions/runs?head_sha=' + 'd' * 40 + '&per_page=100'
                routes[runs_key]['workflow_runs'].insert(0 if position == 'first' else 1, queued)
                routes[runs_key]['total_count'] += 1
                attempt = {**queued, 'run_started_at': None, 'updated_at': '2026-11-09T03:20:00Z'}
                routes[prefix + 'actions/runs/1998/attempts/1'] = ValueError('404') if mode == 'unavailable_attempt' else attempt
                snapshot = self.P.observe(self.options, opener=Transport(routes))
                self.assertFalse(snapshot['complete'])
                failure, = snapshot['failures']
                self.assertEqual(failure['job_id'], 403, (position, mode, snapshot['omissions']))
                self.assertTrue(failure['release_blocking'])

    def test_missing_success_completion_cannot_close_failure(self):
        routes = production_routes()
        prefix = 'repos/octo/project/'
        routes[prefix + 'actions/runs/301/attempts/2/jobs?per_page=100']['jobs'][0]['completed_at'] = '2026-11-09T03:10:00Z'
        add_production_job(routes, 'd' * 40, 302, 1, 404, 'success',
                           '2026-11-09T03:00:00Z', '2026-11-09T03:30:00Z',
                           labels=['macos-14', 'ARM64'])
        jobs_key = prefix + 'actions/runs/302/attempts/1/jobs?per_page=100'
        del routes[jobs_key]['jobs'][0]['completed_at']
        snapshot = self.P.observe(self.options, opener=Transport(routes))
        self.assertFalse(snapshot['complete'])
        failure, = snapshot['failures']
        self.assertEqual(failure['job_id'], 403)
        self.assertTrue(failure['release_blocking'])

    def test_unfinished_workflow_retains_its_completed_failed_job_and_alert(self):
        routes = production_routes()
        prefix = 'repos/octo/project/'
        routes[prefix + 'actions/runs?head_sha=' + 'd' * 40 + '&per_page=100']['workflow_runs'][0]['status'] = 'in_progress'
        routes[prefix + 'actions/runs/301/attempts/2']['status'] = 'in_progress'
        jobs_key = prefix + 'actions/runs/301/attempts/2/jobs?per_page=100'
        routes[jobs_key]['jobs'][0]['completed_at'] = '2026-11-09T03:01:00Z'
        snapshot = self.P.observe(self.options, opener=Transport(routes))
        self.assertFalse(snapshot['complete'])
        self.assertTrue(any('unfinished workflow run' in reason for reason in snapshot['omissions']))
        failure, = snapshot['failures']
        self.assertEqual(failure['job_id'], 403)
        self.assertTrue(failure['release_blocking'])

    def test_unfinished_siblings_retain_completed_release_failure_and_alert(self):
        routes = production_routes()
        prefix = 'repos/octo/project/'
        checks_key = prefix + 'commits/' + 'd' * 40 + '/check-runs?filter=latest&per_page=100'
        routes[checks_key]['check_runs'].append({'id': 1999, 'head_sha': 'd' * 40,
            'name': 'still-running', 'status': 'in_progress', 'conclusion': None,
            'output': {'annotations_count': 0}})
        routes[checks_key]['total_count'] += 1
        runs_key = prefix + 'actions/runs?head_sha=' + 'd' * 40 + '&per_page=100'
        routes[runs_key]['workflow_runs'].append({'id': 1998, 'workflow_id': 1,
            'run_attempt': 1, 'head_sha': 'd' * 40, 'status': 'in_progress',
            'path': '.github/workflows/validate.yml', 'created_at': '2026-11-09T03:20:00Z',
            'repository': {'full_name': 'octo/project'}})
        routes[runs_key]['total_count'] += 1
        snapshot = self.P.observe(self.options, opener=Transport(routes))
        self.assertFalse(snapshot['complete'])
        self.assertTrue(any('unfinished check-run' in reason for reason in snapshot['omissions']))
        self.assertTrue(any('unfinished workflow run' in reason for reason in snapshot['omissions']))
        failure, = snapshot['failures']
        self.assertEqual(failure['job_id'], 403)
        self.assertTrue(failure['release_blocking'])


if __name__ == '__main__':
    unittest.main()
