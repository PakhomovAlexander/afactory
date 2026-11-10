#!/usr/bin/env python3
"""Render an inert Phase 1 native Scenario from an exact snapshot and pinned Task target.

No command execution, schedule, credentials, remote transport or publication. The installed
observer must read back the exact snapshot before the Task can run. Native structural
acceptance checks its one diagnosis against that Task's exact captured snapshot input.
"""
import json
from pathlib import Path
import sys
sys.dont_write_bytecode = True


def lit(value):
    kind = 'boolean' if type(value) is bool else 'integer' if type(value) is int else 'text'
    return {'source': 'literal', 'value': {'type': kind, 'value': value}}


def previous(step):
    return {'source': 'step', 'step': step, 'port': 'result'}


def eq(left, right):
    return {'source': 'equals', 'left': left, 'right': right}


def choose(condition, yes, no):
    return {'source': 'select', 'condition': condition, 'when_true': yes, 'when_false': no}


def all_of(values):
    result = lit(True)
    for value in reversed(values):
        result = choose(value, result, lit(False))
    return result


def not_(value):
    return eq(value, lit(False))


def any_of(values):
    return not_(all_of([not_(v) for v in values]))


def field(name):
    return {'source': 'project', 'value': previous('diagnose'), 'projection': {
        'kind': 'ci_diagnosis_field', 'field': name}}


def refused():
    return {'source': 'project', 'value': previous('diagnose'),
            'projection': {'kind': 'ci_diagnosis_refused'}}


def step(name, operation, inputs, kind, when=None):
    return {'id': name, 'operation': operation, 'inputs': inputs, 'outputs': {'result': kind},
            'when': when, 'max_attempts': 1, 'timeout_ms': 600000, 'on_timeout': 'fail'}


def scope_inputs(snapshot, main_ref='main', observation_start=None, notification='fixture:ci-health'):
    import datetime
    captured = datetime.datetime.strptime(snapshot['captured_at'], '%Y-%m-%dT%H:%M:%SZ')
    start = observation_start or (captured - datetime.timedelta(days=1)).strftime('%Y-%m-%dT%H:%M:%SZ')
    return {key: {'type': 'text', 'value': value} for key, value in {
        'repository': snapshot['repository'], 'main_ref': main_ref,
        'release_selector': str(snapshot['release_pr']['number']) if snapshot['release_pr'] else 'none',
        'observation_start': start, 'observation_end': snapshot['captured_at'],
        'notification': notification}.items()}


def bundle(snapshot, snapshot_id, target, run_id='ci-health-fixture', *, exhausted=False,
           reserved=False, gate_evidence=None, small_repair=False, open_pr=False, watch=False,
           known_transients=(), infrastructure=(), main_ref='main', observation_start=None,
           notification='fixture:ci-health'):
    destination = notification
    concrete = scope_inputs(snapshot, main_ref, observation_start, notification)
    scope_guard = all_of([eq({'source': 'input', 'port': key}, lit(value['value'])) for key, value in concrete.items()])
    grants = [{'adapter': 'ci-health', 'operation': op, 'destination': destination} for op in ('observe', 'notify')]
    observe, notify = grants
    subject, revision = snapshot['repository'], snapshot['source_sha']
    context = {'schema': 'af.ci-health-notification/2', 'scenario_run': run_id, 'snapshot_id': snapshot_id,
               'repository': subject, 'source_sha': revision, 'phase': 'findings', 'report_only': True,
               'complete': snapshot['complete'], 'omissions': snapshot['omissions'],
               'failures': [], 'release_alerts': [],
               'main_ref': main_ref, 'release_selector': concrete['release_selector']['value'],
               'observation_start': concrete['observation_start']['value'],
               'observation_end': snapshot['captured_at'], 'notification': notification}
    wait = lambda condition: {'kind': 'wait', 'grant': observe, 'condition': condition}
    action = {'kind': 'action', 'grant': notify, 'requires_decision': False, 'required_condition': None}
    selected = snapshot['failures'][0] if snapshot['failures'] else None
    if selected is not None:
        context['source_sha'] = selected['head_sha']
    import importlib.util
    root = Path(__file__).resolve().parents[3]
    spec = importlib.util.spec_from_file_location('ci_evidence', root / 'scripts/ci-health.py')
    observer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(observer)
    observer.validate_snapshot(snapshot)
    observer.require(len(known_transients) <= 100 and len(infrastructure) <= 100,
                     'Classifier signature bound exceeded')
    deterministic, diagnose = (None, False)
    if selected is not None:
        deterministic, diagnose = observer.diagnose(snapshot_id, snapshot, selected,
            known_transients=known_transients, infrastructure=infrastructure)
        if exhausted:
            deterministic = observer.malformed(snapshot_id, snapshot, selected, 'diagnosis reservation bound exhausted')
            diagnose = False
    if diagnose:
        deterministic = None
    eligible = False
    if gate_evidence is not None and selected is not None and diagnose:
        eligible = observer.reviewed_gate_count(gate_evidence['documents'], gate_evidence['inventory'],
            selected, snapshot['captured_at'], gate_evidence['evidence_platform']) >= 2
    context.update(occurrence_id=selected['occurrence_id'] if selected else None,
                   signature_id=selected['signature_id'] if selected else None,
                   category=deterministic['category'] if deterministic else None,
                   deterministic_diagnosis=deterministic,
                   unselected_occurrences=[f['occurrence_id'] for f in snapshot['failures'][1:]],
                   unselected_reason='diagnosis_reservation_exhausted' if len(snapshot['failures']) > 1 else None)
    inputs = {'subject': {'source': 'input', 'port': 'repository'}, 'revision': lit(revision)}
    steps = [step('observe', wait('ci_observe_v1'), inputs, 'observation')]
    # Send every observed failure before any model work. Each release-blocking occurrence
    # has an explicit alert in this immediate notification, regardless of diagnosis capacity.
    # Batch against the actual adapter request bound, including escaped body text and scope.
    def finding_message(batch):
        unselected = [f['occurrence_id'] for f in batch['failures']
                      if f['occurrence_id'] != context['occurrence_id']]
        return {**context, **batch, 'unselected_occurrences': unselected,
                'unselected_reason': 'diagnosis_reservation_exhausted' if unselected else None,
                'release_alerts': [f['occurrence_id'] for f in batch['failures'] if f['release_blocking']]}

    def finding_request_bytes(batch):
        body = observer.canonical(finding_message(batch)).decode()
        request_inputs = {key: {'type': 'text', 'value': value}
                          for key, value in {'subject': subject, 'revision': revision, 'body': body}.items()}
        # Reconcile is the longer of the two Action methods. Native intent/input digests
        # always have this fixed width; every other byte is the concrete request's value.
        return len(observer.canonical({'schema': 'af/scenario-adapter-request@4',
            'operation': 'notify', 'method': 'reconcile', 'intent_id': 'sha256:' + '0' * 64,
            'input_digest': 'sha256:' + '0' * 64, 'destination': destination, 'inputs': request_inputs}))

    batches = [{'failures': [], 'omissions': []}]
    for key, items in [('failures', observer.failure_summaries(snapshot)), ('omissions', snapshot['omissions'])]:
        for item in items:
            candidate = {**batches[-1], key: batches[-1][key] + [item]}
            if finding_request_bytes(candidate) > observer.MAX_BYTES:
                batches.append({'failures': [], 'omissions': []})
                batches[-1][key].append(item)
            else:
                batches[-1] = candidate
    for index, batch in enumerate(batches):
        message = finding_message(batch)
        observer.require(finding_request_bytes(batch) <= observer.MAX_BYTES, 'Findings notification request bound exceeded')
        body = observer.canonical(message).decode()
        steps.append(step('notify-findings' + (f'-{index + 1}' if index else ''), action,
                          {**inputs, 'body': lit(body)}, 'action'))
    if diagnose:
        steps.append(step('diagnose', {'kind': 'ci_diagnosis', 'target': target}, {}, 'observation'))
    findings = {**inputs}
    if diagnose:
        findings.update(diagnosis_settlement=previous('diagnose'), category=field('category'), signature_id=field('signature_id'))
        context.update(phase='diagnosis')
        steps.append(step('notify-diagnosis', action, {**findings,
            'body': lit(observer.canonical(context).decode())}, 'action'))
    if diagnose:
        predicates = [any_of([lit(exhausted), eq(field('category'), lit('unknown')),
                              not_(eq(field('signature_id'), lit(selected['signature_id'])))]),
            lit(selected['release_blocking']), eq(field('category'), lit('ci_infra')),
            all_of([eq(field('category'), lit('transient')), lit(not reserved)]),
            all_of([eq(field('category'), lit('test_bug')), lit(eligible)]),
            all_of([any_of([eq(field('category'), lit('product_bug')), eq(field('category'), lit('test_bug'))]),
                    lit(small_repair and not open_pr)]), lit(True)]
        for i, predicate in enumerate(predicates, 1):
            intended = ['alert_only','release_alert','infrastructure_alert','rerun_intended',
                        'quarantine_evidence_intended','repair_intended','report_only'][i-1]
            context.update(phase='remedy-intent', row=i, intended=intended)
            steps.append(step('remedy-' + str(i), action, {**findings,
                'body': lit(json.dumps(context, sort_keys=True, separators=(',', ':')))}, 'action',
                all_of([predicate] + [not_(p) for p in predicates[:i-1]])))
    elif deterministic is not None:
        row, intended = observer.remedy(snapshot, selected, deterministic, exhausted=exhausted,
            rerun_reserved=reserved, small_repair=small_repair, open_pr=open_pr)
        context.update(phase='remedy-intent', row=row, intended=intended)
        steps.append(step('remedy-' + str(row), action, {**findings,
            'body': lit(json.dumps(context, sort_keys=True, separators=(',', ':')))}, 'action'))
    context = {k: v for k, v in context.items() if k not in ('row', 'intended')}
    context.update(phase='outcome')
    steps.append(step('report-outcome', action, {**findings,
        'body': lit(json.dumps(context, sort_keys=True, separators=(',', ':')))}, 'action'))
    # Phase 1 creates no PR, so this is false in every shipped definition. A hermetic service
    # fixture can supply a previously retained owned PR receipt to exercise lifecycle ordering.
    watch_condition = all_of([lit(watch), not_(refused())]) if diagnose else lit(watch)
    steps.append(step('watch', wait('merged'), inputs, 'observation', watch_condition))
    for declared_step in steps:
        declared_step['when'] = all_of([scope_guard, declared_step['when'] or lit(True)])
    return {'schema': 'af/scenario-bundle@6', 'root': 'maintenance-ci-health', 'definitions': {
        'maintenance-ci-health': {'schema': 'af/scenario@6', 'admission': None, 'inputs': {key: 'text' for key in concrete},
            'outputs': {'outcome': 'action'}, 'steps': steps, 'result': {'outcome': previous('report-outcome')},
            'completion': [scope_guard], 'authority': {'task_plans': [target['plan_id']] if diagnose else [],
                'task_templates': [], 'grants': grants, 'decision_owners': []},
            'limits': {'max_tokens': 5096, 'max_wall_ms': 172800000, 'max_dispatches': 20}}}}


def adapter_bytes(root):
    """A test installs one self-contained executable; the machine pins all helper bytes."""
    validator = (root / 'scripts/backlog-check-triage.py').read_text()
    observer = (root / 'scripts/ci-health.py').read_text()
    adapter = (root / 'fixtures/scenarios/maintenance-ci-health/adapter.py').read_text()
    return ('#!/usr/bin/env python3\nimport types\n'
        + "_SCHEMA=types.ModuleType('ci_schema');_SCHEMA.__file__=__file__\n"
        + 'exec(' + repr(validator) + ',_SCHEMA.__dict__)\n'
        + "CI_MODULE=types.ModuleType('ci_health');CI_MODULE.__file__=__file__\n"
        + 'exec(' + repr(observer) + ',CI_MODULE.__dict__)\n'
        + 'CI_MODULE._CI_SCHEMA=_SCHEMA\n' + adapter).encode()


if __name__ == '__main__':
    snapshot, snapshot_id, target, output = sys.argv[1:]
    captured = json.loads(Path(snapshot).read_text())
    Path(output).write_text(json.dumps(bundle(captured, snapshot_id,
        json.loads(Path(target).read_text())), indent=2) + '\n')
    Path(output).with_suffix('.inputs.json').write_text(json.dumps(scope_inputs(captured), indent=2) + '\n')
