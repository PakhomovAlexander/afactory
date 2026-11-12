#!/usr/bin/env python3
"""Credential-free installed ci_observe_v1 and notification service fixture.

A test installs a private copy outside the checkout, pins its full bytes and stages exact
snapshot/native Task evidence. Operations are limited to observe and fixture notifications.
There is no GitHub transport or remedy dispatcher in this executable.
"""
import hashlib
import json
import os
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parent
LIMIT = 65536
raw = sys.stdin.buffer.read(LIMIT + 1)
if len(raw) > LIMIT:
    raise SystemExit('adapter request bound exceeded')
request = CI_MODULE.loads(raw)
if request['schema'] != 'af/scenario-adapter-request@4' or request['destination'] != 'fixture:ci-health':
    raise SystemExit('unsupported protocol or destination')
if request['operation'] not in ('observe', 'notify'):
    raise SystemExit('Phase 1 refuses mutation')
inputs = request['inputs']
config = CI_MODULE.loads((ROOT / 'service.json').read_text())
response = {'schema': 'af/scenario-adapter-response@1', 'intent_id': request['intent_id'],
    'input_digest': request['input_digest'], 'outcome': 'absent',
    'evidence': 'authoritative absence in hermetic notification service', 'observation': None}
native = None
if request['operation'] == 'notify' and 'diagnosis_settlement' in inputs:
    transport = inputs['diagnosis_settlement']
    if set(transport) != {'type', 'value'} or transport['type'] != 'text':
        raise SystemExit('notification requires versioned native diagnosis evidence')
    native = CI_MODULE.loads(transport['value'])
    CI_MODULE.structural(native, 'scenario-ci-notification-evidence')
    if native['repository_id'] != CI_MODULE.digest(config['snapshot']['repository']):
        raise SystemExit('native notification repository reference differs')
body_reference = None
if request['operation'] == 'notify':
    preview = CI_MODULE.loads(inputs['body']['value'])
    body_reference = preview.get('repository_id')
    if body_reference is not None and body_reference != CI_MODULE.digest(config['snapshot']['repository']):
        raise SystemExit('notification repository reference differs')
subject_matches = inputs['subject']['value'] == (
    CI_MODULE.digest(config['snapshot']['repository']) if native or body_reference else config['snapshot']['repository'])
if inputs['revision']['value'] != config['snapshot']['source_sha'] or not subject_matches:
    response.update(outcome='failed', evidence='exact repository/revision precondition refused')
elif request['method'] == 'observe' and request['operation'] == 'observe':
    requested_condition = inputs['condition']['value']
    condition = requested_condition.split(':', 1)[0]
    if condition == 'ci_observe_v1' and requested_condition != 'ci_observe_v1:' + CI_MODULE.digest(config['snapshot']):
        raise SystemExit('observation snapshot digest refused')
    if condition not in ('ci_observe_v1', 'merged'):
        raise SystemExit('unsupported observation')
    with (ROOT / 'events').open('a') as stream:
        stream.write(json.dumps({'phase': condition}) + '\n')
    if condition == 'ci_observe_v1':
        journal = CI_MODULE.Journal(ROOT / 'observer.sqlite')
        try:
            service = config['observation_service']
            if 'github_service' in config:
                fixture = config['github_service']
                client = CI_MODULE.OfflineGithub(fixture['repository'], fixture['routes'], fixture['max_requests'])
                collected = CI_MODULE.collect(client, captured_at=service['captured_at'],
                    main_ref=fixture['main_ref'], since=fixture['since'], nightly_receipt=service['nightly_receipt'],
                    release_pr=service['release_pr']['number'] if service['release_pr'] else None)
                collected['omissions'].extend(service['omissions'])
                service = collected
            snapshot = CI_MODULE.observe(service, journal)
        finally:
            journal.close()
        if snapshot != config['snapshot']:
            raise SystemExit('read-only observation differs from the exact pinned Task snapshot')
    evidence = {'snapshot_id': config['snapshot_id'], 'source_sha': config['snapshot']['source_sha'],
                'complete': config['snapshot']['complete'], 'omissions': config['snapshot']['omissions']}

    response.update(outcome='done', evidence=json.dumps(evidence, separators=(',', ':')),
        observation={'subject': inputs['subject']['value'], 'revision': inputs['revision']['value'],
            'condition': requested_condition, 'satisfied': condition == 'ci_observe_v1' or config['merged'],
            'evidence': json.dumps(evidence, separators=(',', ':'))})
elif request['operation'] == 'notify' and request['method'] in ('execute', 'reconcile'):
    body = json.loads(inputs['body']['value'])
    subject = config['snapshot']['repository']
    if body.get('repository', subject) != subject:
        raise SystemExit('notification repository differs from exact request subject')
    # Every phase binds repository scope by digest in transport. Retain it
    # explicitly in durable notifications for readers without that transport envelope.
    body['repository'] = subject
    if 'findings_reference' in body:
        if (body['findings_reference'] != config['snapshot_id'] or body['phase'] == 'findings'
                or body['omissions']):
            raise SystemExit('notification findings reference differs')
        body['omissions'] = config['snapshot']['omissions']
    CI_MODULE.structural(body, 'ci-health-notification')
    if body['snapshot_id'] != config['snapshot_id'] or body['scenario_run'] != config['scenario_run']:
        raise SystemExit('notification lost exact Run/snapshot identity')
    selected = next((f for f in config['snapshot']['failures'] if f['occurrence_id'] == body['occurrence_id']), None)
    if config['snapshot']['failures'] and selected is None:
        raise SystemExit('notification selected occurrence absent from exact snapshot')
    if body['source_sha'] != (selected['head_sha'] if selected else config['snapshot']['source_sha']):
        raise SystemExit('notification selected revision mismatch')
    summaries = CI_MODULE.failure_summaries(config['snapshot'])
    if (any(f not in summaries for f in body['failures'])
            or body['release_alerts'] != [f['occurrence_id'] for f in body['failures'] if f['release_blocking']]):
        raise SystemExit('notification failure evidence or release alerts differ from exact snapshot')
    if body.get('deterministic_diagnosis') is not None:
        CI_MODULE.validate_diagnosis(body['deterministic_diagnosis'], config['snapshot_id'], config['snapshot'])
    seen = set()
    for result in body['deterministic_results']:
        diagnosis = result['diagnosis']
        CI_MODULE.validate_diagnosis(diagnosis, config['snapshot_id'], config['snapshot'])
        occurrence = diagnosis['occurrence_id']
        if occurrence in seen:
            raise SystemExit('deterministic result occurrence is duplicated')
        seen.add(occurrence)
        intended = ['alert_only', 'release_alert', 'infrastructure_alert', 'rerun_intended',
                    'quarantine_evidence_intended', 'repair_intended', 'report_only'][result['row'] - 1]
        if result['intended'] != intended:
            raise SystemExit('deterministic remedy row/intent mismatch')
    if native is not None:
        if (native['snapshot_id'] != config['snapshot_id'] or selected is None
                or any(native['selection'][key] != selected[key]
                       for key in ('occurrence_id', 'signature_id'))
                or native['selection']['source_sha'] != selected['head_sha']):
            raise SystemExit('notification selected native identity differs')
        if native['provenance']['kind'] == 'deterministic_refusal':
            refusals = native['provenance']['refusals']
            if (native['category'] != 'unknown'
                    or len({r['attempt_id'] for r in refusals}) != len(refusals)):
                raise SystemExit('invalid native refusal references')
            body['deterministic_diagnosis'] = {
                'schema': 'af.ci-diagnosis/1', 'snapshot_id': native['snapshot_id'],
                'source_artifact_ids': [native['snapshot_id']],
                **{key: native['selection'][key] for key in ('occurrence_id', 'signature_id', 'source_sha')},
                'category': 'unknown',
                'rationale': 'Deterministic unknown: native diagnosis Task output was refused',
                'evidence': [{'snapshot_id': native['snapshot_id'],
                    'source_sha': selected['head_sha'], 'reference': 'snapshot'}]}
            CI_MODULE.validate_diagnosis(body['deterministic_diagnosis'], config['snapshot_id'], config['snapshot'])
        if (inputs['category']['value'] != native['category']
                or inputs['signature_id']['value'] != native['selection']['signature_id']):
            raise SystemExit('notification settlement projection mismatch')
        body.update(category=native['category'], signature_id=native['selection']['signature_id'],
                    native_evidence=native)
        CI_MODULE.structural(body, 'ci-health-notification')
    if 'diagnosis' in inputs:
        result = inputs['diagnosis']['value']['result']
        if result['acceptance'] != 'satisfied' or result['execution'] != 'completed':
            raise SystemExit('notification lacks accepted diagnosis')
        if inputs['signature_id']['value'] != selected['signature_id']:
            raise SystemExit('notification signature mismatch')
        body.update(category=inputs['category']['value'], signature_id=inputs['signature_id']['value'])
        CI_MODULE.structural(body, 'ci-health-notification')
    key = hashlib.sha256(request['intent_id'].encode()).hexdigest()
    receipt = ROOT / (key + '.json')
    if receipt.exists():
        response = json.loads(receipt.read_text())
        if response['input_digest'] != request['input_digest']:
            raise SystemExit('changed notification idempotency input')
    elif request['method'] == 'execute':
        response.update(outcome='done', evidence='durable report-only fixture notification: ' + body['phase'])
        with receipt.open('x') as stream:
            json.dump(response, stream)
            stream.flush()
            os.fsync(stream.fileno())
        with (ROOT / 'events').open('a') as stream:
            stream.write(json.dumps(body) + '\n')
else:
    raise SystemExit('unsupported method')
print(json.dumps(response, separators=(',', ':')))
