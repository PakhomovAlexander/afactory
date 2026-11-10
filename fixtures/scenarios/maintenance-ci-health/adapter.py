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
if inputs['revision']['value'] != config['snapshot']['source_sha'] or inputs['subject']['value'] != config['snapshot']['repository']:
    response.update(outcome='failed', evidence='exact repository/revision precondition refused')
elif request['method'] == 'observe' and request['operation'] == 'observe':
    condition = inputs['condition']['value']
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
            'condition': condition, 'satisfied': condition == 'ci_observe_v1' or config['merged'],
            'evidence': json.dumps(evidence, separators=(',', ':'))})
elif request['operation'] == 'notify' and request['method'] in ('execute', 'reconcile'):
    body = json.loads(inputs['body']['value'])
    CI_MODULE.structural(body, 'ci-health-notification')
    if body['snapshot_id'] != config['snapshot_id'] or body['scenario_run'] != config['scenario_run']:
        raise SystemExit('notification lost exact Run/snapshot identity')
    selected = config['snapshot']['failures'][0] if config['snapshot']['failures'] else None
    if body['source_sha'] != (selected['head_sha'] if selected else config['snapshot']['source_sha']):
        raise SystemExit('notification selected revision mismatch')
    summaries = CI_MODULE.failure_summaries(config['snapshot'])
    if (any(f not in summaries for f in body['failures'])
            or body['release_alerts'] != [f['occurrence_id'] for f in body['failures'] if f['release_blocking']]):
        raise SystemExit('notification failure evidence or release alerts differ from exact snapshot')
    if body['deterministic_diagnosis'] is not None:
        CI_MODULE.validate_diagnosis(body['deterministic_diagnosis'], config['snapshot_id'], config['snapshot'])
    if 'diagnosis_settlement' in inputs:
        observation = inputs['diagnosis_settlement']['value']
        record = CI_MODULE.loads(observation['evidence'])
        if (observation['condition'] != 'ci_diagnosis_settled_v1' or not observation['satisfied']
                or record['schema'] != 'af/scenario-ci-diagnosis@2'
                or record['snapshot_id'] != config['snapshot_id']):
            raise SystemExit('notification lacks native diagnosis settlement')
        diagnosis = record['diagnosis']
        CI_MODULE.validate_diagnosis(diagnosis, config['snapshot_id'], config['snapshot'])
        selected = config['snapshot']['failures'][0]
        if any(diagnosis[key] != selected[key] for key in ('occurrence_id', 'signature_id')):
            raise SystemExit('notification selected occurrence/signature mismatch')
        if record['provenance']['kind'] == 'deterministic_refusal':
            if (record['receipt']['result']['acceptance'] == 'satisfied'
                    or diagnosis['category'] != 'unknown' or not record['provenance']['refusals']):
                raise SystemExit('refused diagnosis cannot manufacture acceptance')
            body['deterministic_diagnosis'] = diagnosis
        elif record['provenance']['kind'] == 'accepted':
            result = record['receipt']['result']
            if result['acceptance'] != 'satisfied' or result['execution'] != 'completed':
                raise SystemExit('notification lacks accepted diagnosis')
        else:
            raise SystemExit('unsupported diagnosis provenance')
        if (inputs['category']['value'] != diagnosis['category']
                or inputs['signature_id']['value'] != diagnosis['signature_id']):
            raise SystemExit('notification settlement projection mismatch')
        body.update(category=diagnosis['category'], signature_id=diagnosis['signature_id'])
        CI_MODULE.structural(body, 'ci-health-notification')
    if 'diagnosis' in inputs:
        result = inputs['diagnosis']['value']['result']
        if result['acceptance'] != 'satisfied' or result['execution'] != 'completed':
            raise SystemExit('notification lacks accepted diagnosis')
        if inputs['signature_id']['value'] != config['snapshot']['failures'][0]['signature_id']:
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
