"""Fail-closed native evidence for configure()'s fixed two-step recovery Scenario.

This is a targeted projection of scenario-v11 / scenario-run-v12 and TaskResultV1,
with an explicit matched tag set for retained complete v11 evidence. It is
not a JSON Schema evaluator or an execution-authority validator. Unsupported
Scenario shapes cannot witness these four Checks faults. Keep native contract
parity and captured-receipt mutation tests beside changes to this projection.
"""
from hashlib import sha256
import json
import re

SAFE = 2 ** 53 - 1


def require(condition):
    if not condition:
        raise ValueError("incomplete or inconsistent native recovery evidence")


def fields(value, names):
    require(type(value) is dict and set(value) == set(names.split()))


def integer(value, low=0, high=SAFE):
    require(type(value) is int and low <= value <= high)


def text(value):
    require(type(value) is str and 0 < len(value.encode()) <= 65536 and '\0' not in value)


def path(value):
    text(value)
    require(value.startswith("/") and len(value) <= 4096)


def name(value):
    require(type(value) is str and re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,127}', value))


def digest(value):
    require(type(value) is str and re.fullmatch(r'sha256:[0-9a-f]{64}', value))


def native_hash(value):
    # All keys in this fixed projection are ASCII; native canonical JSON sorts
    # keys by UTF-16. Values contain no floats, and strings retain UTF-8.
    return 'sha256:' + sha256(json.dumps(value, sort_keys=True, ensure_ascii=False,
                                        separators=(',', ':'), allow_nan=False).encode()).hexdigest()


def limits(value):
    fields(value, 'max_tokens max_wall_ms max_dispatches')
    integer(value['max_tokens'])
    integer(value['max_wall_ms'], 1, 8640000000000)
    integer(value['max_dispatches'], 1, 10000)


def task_receipt(datum):
    fields(datum, 'type value')
    require(datum['type'] == 'task')
    receipt = datum['value']
    fields(receipt, 'task_id plan_id revision_id result_id charged_tokens result')
    name(receipt['task_id'])
    for key in ('plan_id', 'revision_id', 'result_id'):
        digest(receipt[key])
    integer(receipt['charged_tokens'], 0, 2 ** 64 - 1)
    result = receipt['result']
    fields(result, 'task_revision_id execution acceptance domain_conclusion outputs evidence missing_obligations')
    require(result['task_revision_id'] == receipt['revision_id']
            and result['execution'] == 'completed' and result['acceptance'] == 'satisfied')
    text(result['domain_conclusion'])
    require(result['domain_conclusion'].strip())
    require(type(result['missing_obligations']) is list and not result['missing_obligations'])
    evidence = result['evidence']
    require(type(evidence) is list and evidence)
    for item in evidence:
        digest(item)
    require(len(set(evidence)) == len(evidence))
    outputs = result['outputs']
    require(type(outputs) is dict and 'snapshot' in outputs)
    for port, output in outputs.items():
        name(port)
        require(type(output) is dict)
        fields(output, 'artifact_ids artifact_type cardinality' +
               (' snapshot_id' if 'snapshot_id' in output else ''))
        require(type(output['artifact_type']) is str and re.fullmatch(
            r'[a-z][a-z0-9.]*/[A-Za-z][A-Za-z0-9]*@[1-9][0-9]*', output['artifact_type']))
        require(output['cardinality'] in ('one', 'many'))
        ids = output['artifact_ids']
        require(type(ids) is list and ids)
        for item in ids:
            digest(item)
        require(len(set(ids)) == len(ids) and (output['cardinality'] == 'many' or len(ids) == 1))
        if 'snapshot_id' in output:
            digest(output['snapshot_id'])
    snapshot = outputs['snapshot']
    require(snapshot['artifact_type'] == 'af/SourceTree@1' and snapshot['cardinality'] == 'one')
    digest(snapshot['snapshot_id'])
    return receipt, snapshot['snapshot_id']


def pinned_steps(pin, version):
    """Validate the complete fixed native definition and its policy and hashes."""
    fields(pin, 'schema bundle policy revision definitions')
    require(pin['schema'] == f'af/scenario-pin@{version}')
    bundle, policy = pin['bundle'], pin['policy']
    fields(bundle, 'schema root definitions')
    require(bundle['schema'] == f'af/scenario-bundle@{version - 1}' and bundle['root'] == 'clean')
    fields(bundle['definitions'], 'clean')
    definition = bundle['definitions']['clean']
    fields(definition, 'schema inputs outputs steps result completion authority limits admission')
    limits(definition['limits'])
    steps = definition['steps']
    require(type(steps) is list and len(steps) == 2)
    target = steps[0]['operation']['target']
    fields(target, 'repo state task_id plan_id revision_id max_tokens')
    for key in ('repo', 'state'):
        path(target[key])
    name(target['task_id'])
    digest(target['plan_id'])
    digest(target['revision_id'])
    integer(target['max_tokens'])
    require(target['max_tokens'] <= definition['limits']['max_tokens'])
    candidate = {'source': 'step', 'step': 'implement', 'port': 'result'}
    grant = {'adapter': 'checks', 'operation': 'experiment_submit_v1', 'destination': 'fixture:checks'}
    expected_steps = [
        {'id': 'implement', 'operation': {'kind': 'task', 'target': target},
         'inputs': {}, 'outputs': {'result': 'task'}, 'when': None,
         'max_attempts': 1, 'timeout_ms': 90000, 'on_timeout': 'fail'},
        {'id': 'checks', 'operation': {'kind': 'action', 'grant': grant,
                                     'requires_decision': False, 'required_condition': None},
         'inputs': {'body': {'source': 'literal', 'value': {'type': 'text', 'value': 'Fixture CI'}},
                    'candidate': candidate,
                    'revision': {'source': 'project', 'value': candidate,
                                 'projection': {'kind': 'task_output_snapshot', 'port': 'snapshot'}},
                    'subject': {'source': 'literal', 'value': {'type': 'text', 'value': 'pagination'}}},
         'outputs': {'result': 'action'}, 'when': None,
         'max_attempts': 4, 'timeout_ms': 60000, 'on_timeout': 'fail'}]
    expected_definition = {
        'schema': f'af/scenario@{version - 1}', 'inputs': {}, 'outputs': {'candidate': 'task', 'checks': 'action'},
        'steps': expected_steps, 'result': {'candidate': candidate,
                                          'checks': {'source': 'step', 'step': 'checks', 'port': 'result'}},
        'completion': [{'source': 'literal', 'value': {'type': 'boolean', 'value': True}}],
        'authority': {'task_plans': [target['plan_id']], 'task_templates': [],
                      'grants': [grant], 'decision_owners': []},
        'limits': definition['limits'], 'admission': None}
    # Native Scenario::validate bounds each fixed step by the definition wall,
    # independently of the lower limits a Run may select.
    require(all(step['timeout_ms'] <= definition['limits']['max_wall_ms']
                for step in expected_steps))
    # JSON comparison also distinguishes bool from int in these fixed literals.
    require(native_hash(definition) == native_hash(expected_definition))
    fields(policy, 'schema adapters task_plans task_templates decision_keys nightly_releases '
           'performance_publications file_roots implementation_startup')
    fields(policy['adapters'], 'checks')
    adapter = policy['adapters']['checks']
    fields(adapter, 'executable digest operations destinations max_wall_ms max_output_bytes')
    path(adapter['executable'])
    digest(adapter['digest'])
    integer(adapter['max_wall_ms'], 1, 30000)
    integer(adapter['max_output_bytes'], 1, 65536)
    require(adapter['operations'] == ['experiment_submit_v1']
            and adapter['destinations'] == ['fixture:checks'])
    require(policy == {'schema': 'af/scenario-policy@8', 'adapters': {'checks': adapter},
                       'task_plans': [target['plan_id']], 'task_templates': [], 'decision_keys': {},
                       'nightly_releases': {}, 'performance_publications': {}, 'file_roots': [],
                       'implementation_startup': None})
    require(pin['definitions'] == {'clean': native_hash(definition)}
            and pin['revision'] == native_hash([bundle, policy]))
    return definition, target


def before_identity(before):
    fields(before, 'schema runs')
    require(before['schema'] in ('af/scenario-list@11', 'af/scenario-list@12'))
    version = 12 if before['schema'] == 'af/scenario-list@12' else 11
    require(type(before['runs']) is dict and len(before['runs']) == 1)
    run_id, run = next(iter(before['runs'].items()))
    name(run_id)
    fields(run, 'schema id execution_id pin limits created_unix_ms updated_unix_ms status waiting '
           'frames task_charges dispatches failure cancellation ant goal_claim fresh_tasks')
    require(run['schema'] == f'af/scenario-run@{version}' and run['id'] == run_id and run['status'] == 'waiting')
    digest(run['execution_id'])
    definition, target = pinned_steps(run['pin'], version)
    limits(run['limits'])
    require(all(run['limits'][k] <= definition['limits'][k] for k in run['limits']))
    integer(run['created_unix_ms'], 1)
    integer(run['updated_unix_ms'], run['created_unix_ms'])
    require(run['created_unix_ms'] + run['limits']['max_wall_ms'] <= SAFE)
    require(run['failure'] is None and run['cancellation'] is None and run['goal_claim'] is None
            and type(run['fresh_tasks']) is dict and not run['fresh_tasks'])
    name(run['ant'])
    fields(run['frames'], 'root')
    frame = run['frames']['root']
    fields(frame, 'scenario inputs started_unix_ms steps dispatches outputs')
    require(frame['scenario'] == 'clean' and frame['inputs'] == {} and frame['outputs'] is None)
    integer(frame['started_unix_ms'], run['created_unix_ms'], run['updated_unix_ms'])
    require(frame['started_unix_ms'] + definition['limits']['max_wall_ms'] <= SAFE)
    integer(run['dispatches'], 2, run['limits']['max_dispatches'])
    integer(frame['dispatches'], 2, definition['limits']['max_dispatches'])
    require(frame['dispatches'] == run['dispatches'])
    require(type(frame['steps']) is list and len(frame['steps']) == 2)
    implement, checks = frame['steps']
    receipt, revision = task_receipt(implement['outputs']['result'])
    require(all(receipt[k] == target[k] for k in ('task_id', 'plan_id', 'revision_id')))
    inputs = [{}, {'body': {'type': 'text', 'value': 'Fixture CI'},
                   'candidate': implement['outputs']['result'],
                   'revision': {'type': 'text', 'value': revision},
                   'subject': {'type': 'text', 'value': 'pagination'}}]
    for index, progress in enumerate(frame['steps']):
        fields(progress, 'step started_unix_ms deadline_unix_ms status skipped intents outputs child decision diagnostic')
        step = definition['steps'][index]
        require(progress['step'] == step['id'] and progress['status'] == ('succeeded' if index == 0 else 'waiting'))
        integer(progress['started_unix_ms'], frame['started_unix_ms'], run['updated_unix_ms'])
        integer(progress['deadline_unix_ms'], 1, SAFE)
        require(progress['deadline_unix_ms'] == progress['started_unix_ms'] + step['timeout_ms'])
        require(all(progress[k] is None for k in ('skipped', 'child', 'decision', 'diagnostic')))
        fields(progress['outputs'], 'result' if index == 0 else '')
        require(type(progress['intents']) is list and len(progress['intents']) == 1)
        intent, = progress['intents']
        fields(intent, 'id input_digest phase evidence')
        require(intent['id'] == native_hash([run['execution_id'], run['pin']['revision'],
                                             'root/' + step['id'], 1])
                and intent['input_digest'] == native_hash([step, inputs[index]])
                and intent['phase'] == ('done' if index == 0 else 'uncertain'))
        require(type(intent['evidence']) is list)
        for evidence in intent['evidence']:
            text(evidence)
    require(implement['started_unix_ms'] <= checks['started_unix_ms'])
    charge_id = native_hash([target['state'], target['task_id'], target['revision_id']])
    fields(run['task_charges'], charge_id)
    charge = run['task_charges'][charge_id]
    fields(charge, 'maximum charged unknown_usage step')
    integer(charge['maximum'])
    integer(charge['charged'], 0, charge['maximum'])
    require(charge['maximum'] == target['max_tokens'] and charge['charged'] == receipt['charged_tokens']
            and charge['unknown_usage'] is False and charge['step'] == 'root/implement'
            and charge['charged'] <= run['limits']['max_tokens'])
    waiting = run['waiting']
    fields(waiting, 'kind step intent_id diagnostic')
    require(waiting['kind'] == 'reconcile' and waiting['step'] == 'root/checks'
            and waiting['intent_id'] == checks['intents'][0]['id'])
    text(waiting['diagnostic'])
    return revision, waiting['intent_id']
