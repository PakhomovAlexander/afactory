#!/usr/bin/env python3
"""Offline Phase 1 contract, observation, recurrence and remedy acceptance."""
import copy
import importlib.util
import itertools
import json
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
                  'plan_id': 'sha256:' + 'd' * 64, 'revision_id': 'sha256:' + 'e' * 64, 'max_tokens': 5096}
        signature = snapshot['failures'][0]['signature_id']
        for flags, row in [({'known_transients': [signature]}, 4), ({'infrastructure': [signature]}, 3), ({'exhausted': True}, 1)]:
            rendered = module.bundle(snapshot, C.digest(snapshot), target, **flags)
            steps = rendered['definitions']['maintenance-ci-health']['steps']
            self.assertFalse(any(s['operation']['kind'] in ('task', 'ci_diagnosis') for s in steps))
            self.assertEqual([s['id'] for s in steps], ['observe', 'notify-findings', 'remedy-' + str(row), 'report-outcome', 'watch'])
            message = json.loads(steps[1]['inputs']['body']['value']['value'])
            C.structural(message, 'ci-health-notification')
            C.validate_diagnosis(message['deterministic_diagnosis'], C.digest(snapshot), snapshot)
        release = copy.deepcopy(snapshot)
        release['failures'][0]['release_blocking'] = True
        steps = module.bundle(release, C.digest(release), target, known_transients=[signature])['definitions']['maintenance-ci-health']['steps']
        self.assertEqual(steps[2]['id'], 'remedy-2')

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
                sid = C.digest(snapshot)
                (root / 'service.json').write_text(json.dumps({'snapshot': snapshot, 'snapshot_id': sid,
                    'observation_service': raw, 'github_service': github,
                    'scenario_run': 'ci-health-fixture', 'merged': False}))
                executable = root / 'adapter'
                executable.write_bytes(module.adapter_bytes(ROOT))
                executable.chmod(0o700)

                def invoke(operation, method, identity, **values):
                    inputs = {key: {'type': 'text', 'value': value} for key, value in {
                        'subject': snapshot['repository'], 'revision': snapshot['source_sha'], **values}.items()}
                    request = {'schema': 'af/scenario-adapter-request@4', 'operation': operation,
                        'method': method, 'intent_id': C.digest(identity), 'input_digest': C.digest(inputs),
                        'destination': 'fixture:ci-health', 'inputs': inputs}
                    result = subprocess.run([str(executable)], input=C.canonical(request), capture_output=True, check=True)
                    response = C.loads(result.stdout)
                    self.assertEqual(response['intent_id'], request['intent_id'])
                    self.assertEqual(response['input_digest'], request['input_digest'])
                    self.assertEqual(response['schema'], 'af/scenario-adapter-response@1')
                    return response

                observed = invoke('observe', 'observe', 'observe', condition='ci_observe_v1')
                self.assertEqual(observed['outcome'], 'done')
                self.assertTrue(observed['observation']['satisfied'])
                evidence = json.loads(observed['evidence'])
                self.assertEqual(evidence['complete'], case != 'incomplete')
                self.assertEqual(evidence['snapshot_id'], sid)
                target = {'repo': '/fixture/repo', 'state': '/fixture/state', 'task_id': 'diagnose',
                    'plan_id': C.digest('plan'), 'revision_id': C.digest('revision'), 'max_tokens': 5096}
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
                if case == 'unselected-release':
                    self.assertFalse(snapshot['failures'][0]['release_blocking'])
                    release = snapshot['failures'][1]
                    self.assertTrue(release['release_blocking'])
                    self.assertEqual(events[1]['failures'][1]['log_artifacts'], release['log_artifacts'])
                    self.assertEqual(events[1]['unselected_occurrences'], [release['occurrence_id']])
                    self.assertEqual(events[1]['unselected_reason'], 'diagnosis_reservation_exhausted')
                    self.assertFalse(any(s['operation']['kind'] == 'ci_diagnosis' for s in bundle['definitions'][bundle['root']]['steps']))
                old = json.loads(body)
                old['schema'] = 'af.ci-health-notification/1'
                with self.assertRaises(ValueError):
                    C.structural(old, 'ci-health-notification')

    def test_findings_batch_all_observed_failures_omissions_and_release_alerts_within_existing_bounds(self):
        spec = importlib.util.spec_from_file_location('ci_scenario', FIXTURE / 'configure.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        raw = service()
        raw['omissions'] = [str(i) + ' missing evidence ' + 'x' * 230 for i in range(100)]
        raw['jobs'] = [dict(raw['jobs'][0], run_id=i + 1, job_id=i + 1,
            job_name='核' * 200 + str(i), release_blocking=True,
            log_artifacts=['sha256:' + format(j, '064x') for j in range(20)]) for i in range(100)]
        snapshot = self.observe(raw)
        self.assertTrue(snapshot['failures'])
        target = {'repo': '/fixture/repo', 'state': '/fixture/state', 'task_id': 'diagnose',
                  'plan_id': C.digest('plan'), 'revision_id': C.digest('revision'), 'max_tokens': 5096}
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
            inputs['subject'] = {'type': 'text', 'value': snapshot['repository']}
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
        self.assertEqual(json.loads(body)['repository'], healthy['repository'])
        self.assertGreater(len(body.encode()), 16384)
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
        first = self.observe(raw)['failures'][0]
        self.assertEqual(self.observe(raw)['failures'][0]['occurrence_count'], 1)
        raw['jobs'][0]['job_id'] += 1
        self.assertEqual(self.observe(raw)['failures'][0]['occurrence_count'], 1)
        raw['jobs'][0]['run_attempt'] += 1
        self.assertEqual(self.observe(raw)['failures'][0]['occurrence_count'], 2)
        raw['jobs'][0].update(head_sha='b' * 40, run_id=300)
        changed = self.observe(raw)['failures'][0]
        self.assertEqual(changed['signature_id'], first['signature_id'])
        self.assertEqual(changed['occurrence_count'], 3)
        raw['jobs'][0]['conclusion'] = 'success'
        self.assertEqual(self.observe(raw)['failures'], [])
        raw['jobs'][0]['conclusion'] = 'failure'
        self.assertEqual(self.observe(raw)['failures'][0]['occurrence_count'], 1)
        raw['captured_at'] = '2026-11-24T04:00:00Z'
        expired = self.observe(raw)['failures'][0]
        self.assertEqual(expired['occurrence_count'], 1)
        self.assertEqual(expired['first_seen_at'], raw['captured_at'])
        self.journal.close()
        self.journal = C.Journal(Path(self.temp.name) / 'observer.sqlite')
        self.addCleanup(self.journal.close)
        self.assertEqual(self.observe(raw)['failures'][0]['occurrence_count'], 1)

    def test_green_closure_is_order_independent_within_a_capture_and_conflicts_are_unknown(self):
        raw = service()
        passed = copy.deepcopy(raw['jobs'][0])
        passed.update(conclusion='success', run_attempt=2, job_id=202)
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


if __name__ == '__main__':
    unittest.main()
