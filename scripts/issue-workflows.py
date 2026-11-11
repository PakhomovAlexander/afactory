#!/usr/bin/env python3
"""Inspect, plan and reconcile the versioned issue Pipeline catalog. Never execute or publish."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent / 'performance'))
from perflab._vendor import tomli

ROOT = Path(__file__).resolve().parent.parent
STAGED = ROOT / 'fixtures/issue-workflows'
LIMIT = 4 * 1024 * 1024


def read(path):
    data = Path(path).read_bytes()
    if len(data) > LIMIT:
        raise ValueError('input exceeds 4 MiB')
    return json.loads(data)


def toml(path):
    data = Path(path).read_bytes()
    if len(data) > LIMIT:
        raise ValueError('input exceeds 4 MiB')
    return tomli.loads(data.decode())


def exact(obj, required, optional=()):
    if not isinstance(obj, dict) or set(obj) - set(required) - set(optional) or set(required) - set(obj):
        raise ValueError('missing or unknown manifest fields')


def catalog():
    value = read(STAGED / 'selections.json')
    if value['schema'] != 'af.issue-workflows/1':
        raise ValueError('unknown workflow catalog')
    return value


def selection(name):
    choices = catalog()['selections']
    if name not in choices:
        raise ValueError('unknown or retired Pipeline selection: ' + name)
    return choices[name]


def invoke(af, repo, state, *args):
    # No shell, remote adapter, Task run, delivery, credentials or policy changes.
    result = subprocess.run([str(af), 'task', *args, '--repo', str(repo), '--state', str(state), '--json'],
                            check=False, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120)
    if len(result.stdout) > LIMIT:
        raise ValueError('native inspection exceeds 4 MiB')
    value = json.loads(result.stdout)
    if result.returncode and (args[0] not in ('explain', 'list') or value.get('schema') not in ('af/task-inspection@20', 'af/task-list@4')):
        raise ValueError('native command refused; inspect its recorded Task diagnostics')
    return value


def route(name, facts):
    choice = selection(name)
    exact(facts, ('known_cause', 'deterministic_regression', 'narrow_change', 'authority',
                  'contract', 'security', 'accounting', 'persistence', 'uncertain_concurrency'))
    if any(type(v) is not bool for v in facts.values()):
        raise ValueError('routing facts must be explicit booleans')
    sensitive = any(facts[k] for k in ('authority', 'contract', 'security', 'accounting', 'persistence', 'uncertain_concurrency'))
    if choice['light'] and (sensitive or not all(facts[k] for k in ('known_cause', 'deterministic_regression', 'narrow_change'))):
        raise ValueError('light requires known cause, deterministic regression and narrow non-authority scope')
    if not facts['known_cause'] and name.startswith('bugfix/') and 'research-fix' not in name:
        raise ValueError('unknown bug cause requires research-fix')


def validate_plan(name, value):
    """Compare effective native bindings with the exact staged Worker definitions and pins."""
    selection(name)
    native = toml(STAGED / 'replacement-catalog.toml')
    plan, graph = value['plan'], value['graph']
    if value['selection']['assessment']['decision'] != {'kind': 'selected', 'pipeline': name}:
        raise ValueError('native selection differs')
    definition = toml(STAGED / 'task-packages' / name / 'pipeline.toml')
    expected_slots = {'root.slots.' + slot for slot in definition['slots']}
    if set(plan.get('bindings', {})) != expected_slots or set(graph['slots']) != expected_slots:
        raise ValueError('missing or extra effective roles')
    required_packages = set()
    def closure(package):
        if package in required_packages:
            return
        required_packages.add(package)
        file = STAGED / 'task-packages' / package / 'pipeline.toml'
        if file.is_file():
            definition = toml(file)
            for worker in definition['slots'].values():
                closure(worker['worker'])
            for node in definition['nodes']:
                if node['operator']['op'] == 'call':
                    closure(node['operator']['pipeline'])
    closure(name)
    if set(plan['dependencies']) != required_packages:
        raise ValueError('missing or extra Pipeline dependencies')
    for slot, bound in plan['bindings'].items():
        worker = graph['slots'][slot]['worker']
        if worker not in native['providers']:
            raise ValueError('unapproved effective Worker')
        pin = native['packages'][worker]
        definition = toml(STAGED / 'task-packages' / worker / 'worker.toml')
        actual = bound['execution']
        runner = definition['runner']
        if (bound['package_digest'] != pin['digest'] or actual.get('kind') != 'model'
                or any(actual.get(k) != runner[k] for k in ('model', 'effort', 'provider_kind'))
                or actual.get('provider') != native['providers'][worker]):
            raise ValueError('effective role/model/effort/provider or pin differs: ' + slot)
    if name not in plan['dependencies']:
        raise ValueError('missing exact Pipeline dependency')
    for package, dependency in plan['dependencies'].items():
        if package not in native['packages']:
            raise ValueError('unknown Pipeline dependency: ' + package)
        pin = native['packages'][package]
        directory = STAGED / 'task-packages' / package
        files = {p.relative_to(directory).as_posix(): list(p.read_bytes()) for p in sorted(directory.rglob('*')) if p.is_file()}
        payload = {'schema': 'af.task-package/1', 'name': package, 'version': pin['version'], 'digest': pin['digest'], 'files': files}
        canonical = json.dumps(payload, sort_keys=True, ensure_ascii=False, separators=(',', ':')).encode()
        digest = 'sha256:' + hashlib.sha256(b'review.kernel/content-id/v1\0' + canonical).hexdigest()
        if dependency['content_digest'] != digest:
            raise ValueError('Pipeline dependency differs from versioned catalog: ' + package)
    rendered = json.dumps(graph)
    if 'publish-gate' in rendered or 'codex-work' in rendered or any(s.rsplit('.', 1)[-1] in ('uix', 'advisor') for s in graph['slots']):
        raise ValueError('unexpected authority or advisory role')
    # Exact native pipeline pins, rather than a prose stage list, own graph shape/dataflow.
    # The independently compiled graph is checked by af; this validator checks effective roles.
    return {'selection': name, 'plan_id': value['plan_id'], 'revision_id': value['revision_id'],
            'roles': {slot: b['execution'] for slot, b in plan['bindings'].items()}}


def inventory(af, repo, state):
    value = invoke(af, repo, state, 'list')
    if value.get('schema') != 'af/task-list@4':
        raise ValueError('unsupported native inventory')
    return value['tasks']


def migration(af, repo, state):
    tasks = inventory(af, repo, state)
    # This preview never changes history or authority. Captured plans retain their own bytes.
    active = [t['task_id'] for t in tasks if t['phase']['kind'] not in ('finished', 'collected')]
    old = toml(Path(repo) / '.af/task-catalog.toml')
    desired = toml(STAGED / 'replacement-catalog.toml')
    return {'schema': 'af.issue-workflow-migration/1', 'ready': not active, 'active_tasks': active,
            'preserve_tasks': [t['task_id'] for t in tasks],
            'remove_packages': sorted(set(old['packages']) - set(desired['packages'])),
            'replacement_catalog': str(STAGED / 'replacement-catalog.toml'),
            'replacement_settings': str(STAGED / 'replacement-af.toml'),
            'replacement_catalog_sha256': hashlib.sha256((STAGED / 'replacement-catalog.toml').read_bytes()).hexdigest(),
            'default_pipeline': catalog()['default'],
            'verification_pipeline': catalog()['verification'],
            'copy_packages': desired['packages'],
            'bindings_policy': 'providers only; codex-personal and pi-personal; never rewrite captured Tasks'}


def reconcile(af, repo, state, manifest):
    """Native checked Task inspections, never imported spend or acceptance assertions."""
    exact(manifest, ('schema', 'issue', 'pull_request', 'source_commit', 'selection', 'tasks'), ('research_tasks', 'verification_task'))
    if manifest['schema'] != 'af.issue-workflow-pilot/1':
        raise ValueError('unknown pilot schema')
    choice = selection(manifest['selection'])
    if not re.fullmatch(r'https://github.com/[^/]+/[^/]+/issues/[1-9][0-9]*', manifest['issue']):
        raise ValueError('pilot requires exact issue identity')
    pr = manifest['pull_request']
    if pr is not None and not re.fullmatch(r'https://github.com/[^/]+/[^/]+/pull/[1-9][0-9]*', pr):
        raise ValueError('invalid PR identity')
    if not re.fullmatch(r'[0-9a-f]{40}', manifest['source_commit']):
        raise ValueError('pilot requires exact source commit')
    if not isinstance(manifest['tasks'], list) or any(not isinstance(t, str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,127}', t) for t in manifest['tasks']):
        raise ValueError('Task identities must be native names')
    if not manifest['tasks'] or len(manifest['tasks']) > 64 or len(set(manifest['tasks'])) != len(manifest['tasks']):
        raise ValueError('pilot requires a unique bounded cumulative Task chain')
    snapshots, records, total, unknown = [], [], 0, 0
    auxiliary = manifest.get('research_tasks', []) + ([manifest['verification_task']] if manifest.get('verification_task') else [])
    if len(auxiliary) > 16 or len(set(manifest['tasks'] + auxiliary)) != len(manifest['tasks'] + auxiliary):
        raise ValueError('all stage Task identities must be unique and bounded')
    for i, task in enumerate(manifest['tasks']):
        value = invoke(af, repo, state, 'explain', task)
        if value.get('schema') != 'af/task-inspection@20' or value['task_id'] != task:
            raise ValueError('native Task identity differs')
        if i and value.get('continues', {}).get('task_id') != manifest['tasks'][i - 1]:
            raise ValueError('pilot omits or changes cumulative continuation ancestry')
        if not i and value.get('continues'):
            raise ValueError('pilot must include original Task, not only last delta')
        total += int(value['chargeable_tokens'])
        unknown += value.get('unknown_usage_attempts', 0)
        records.append({'task_id': task, 'revision_id': value['revision_id'], 'plan_id': value['plan_id'],
                        'chargeable_tokens': value['chargeable_tokens'], 'state': value['state'],
                        'source_snapshot_id': value.get('plan', {}).get('inputs', {}).get('source', {}).get('snapshot_id'),
                        'input_bindings': value.get('input_bindings')})
        snapshots.append(value.get('continues', {}).get('snapshot_id'))
    for task in auxiliary:
        value = invoke(af, repo, state, 'explain', task)
        if value.get('schema') != 'af/task-inspection@20' or value['task_id'] != task:
            raise ValueError('native stage Task identity differs')
        if value.get('result', {}).get('acceptance') != 'satisfied':
            raise ValueError('stage Task is not independently accepted')
        if task == manifest.get('verification_task'):
            bound = value.get('input_bindings', {}).get('bindings', {}).get('source', {}).get('task', {})
            if bound.get('task_id') != manifest['tasks'][-1]:
                raise ValueError('final verification does not bind the final whole candidate')
        total += int(value['chargeable_tokens'])
        unknown += value.get('unknown_usage_attempts', 0)
        records.append({'task_id': task, 'revision_id': value['revision_id'], 'plan_id': value['plan_id'],
                        'chargeable_tokens': value['chargeable_tokens'], 'state': value['state'],
                        'source_snapshot_id': value.get('plan', {}).get('inputs', {}).get('source', {}).get('snapshot_id'),
                        'input_bindings': value.get('input_bindings')})
    if choice['light'] and len(manifest['tasks']) > 2:
        raise ValueError('light permits one implementation and at most one bounded repair')
    return {'schema': 'af.issue-workflow-reconciliation/1', 'pilot': manifest, 'tasks': records,
            'chain_chargeable_tokens': str(total), 'unknown_usage_attempts': unknown,
            'continuation_sources': snapshots, 'final_verification_recorded': bool(manifest.get('verification_task')),
            'quality_or_savings_claim': False}


def captured_catalog(template):
    commit = template['authority_commit']
    if not re.fullmatch(r'[0-9a-f]{40}', commit):
        raise ValueError('template requires exact source commit')
    result = subprocess.run(['git', '-C', template['repo'], 'show', commit + ':.af/task-catalog.toml'],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30, check=True)
    if len(result.stdout) > LIMIT or tomli.loads(result.stdout.decode()) != toml(STAGED / 'replacement-catalog.toml'):
        raise ValueError('pinned source does not install the exact versioned replacement catalog')


def composition(name, pins, facts):
    """A pure native Scenario: accepted report -> code -> shared read-only verification."""
    route(name, facts)
    choice = selection(name)
    research = choice['research_pipeline']
    expected = ([research] if research else []) + [name, catalog()['verification']]
    if len(pins) != len(expected):
        raise ValueError('supply one exact native template pin per stage: ' + ', '.join(expected))
    templates, identities = [], []
    source = None
    for pin, pipeline in zip(pins, expected):
        exact(pin, ('schema', 'template_id', 'template'))
        if pin['schema'] != 'af/scenario-template-pin@1' or not re.fullmatch(r'sha256:[0-9a-f]{64}', pin['template_id']):
            raise ValueError('expected exact native template-pin receipt')
        template = pin['template']
        captured_catalog(template)
        identity = tuple(template[k] for k in ('repo', 'state', 'authority_commit', 'repository_id', 'source_content_digest'))
        if source is None:
            source = identity
        if source != identity:
            raise ValueError('stage templates must share exact source, authority and Store')
        document = template['document']
        if document.get('pipeline') != {'name': pipeline, 'fallback': 'refuse'}:
            raise ValueError('stage template selects a different Pipeline')
        if pipeline == catalog()['verification'] and document.get('base', {}).get('commit') != template['authority_commit']:
            raise ValueError('verification base must retain the original whole-change commit')
        if template.get('bindings') and set(template['bindings']) - {'schema', 'providers'}:
            raise ValueError('stage templates may bind only personal accounts')
        if template.get('bindings') and any(p not in ('codex-personal', 'pi-personal') for p in template['bindings'].get('providers', {}).values()):
            raise ValueError('stage templates require personal accounts')
        templates.append(template)
        identities.append(pin['template_id'])
    steps = []
    for index, (template, pipeline) in enumerate(zip(templates, expected)):
        stage = 'research' if research and index == 0 else 'verify' if index == len(expected) - 1 else 'implement'
        inputs, bindings = {}, {}
        if stage == 'implement' and research:
            inputs['research_report'] = {'source': 'step', 'step': 'research', 'port': 'result'}
            bindings['research_report'] = {'port': 'report', 'artifact_type': 'af/Document@1'}
        if stage == 'verify':
            inputs['source'] = {'source': 'step', 'step': 'implement', 'port': 'result'}
            bindings['source'] = {'port': 'snapshot', 'artifact_type': 'af/SourceTree@1'}
        steps.append({'id': stage, 'operation': {'kind': 'fresh_task', 'template': template, 'bindings': bindings},
                      'inputs': inputs, 'outputs': {'result': 'task'}, 'when': None, 'max_attempts': 1,
                      'timeout_ms': template['document']['limits']['wall_ms'], 'on_timeout': 'fail'})
    # A Task datum is admitted only for an accepted, completed exact native result. The
    # research_report predecessor additionally authenticates original source and 32 KiB bound.
    definition = {'schema': 'af/scenario@5', 'admission': None, 'inputs': {},
                  'outputs': {s['id']: 'task' for s in steps}, 'steps': steps,
                  'result': {s['id']: {'source': 'step', 'step': s['id'], 'port': 'result'} for s in steps},
                  'completion': [{'source': 'literal', 'value': {'type': 'boolean', 'value': True}}],
                  'authority': {'task_plans': [], 'task_templates': sorted(set(identities)), 'grants': [], 'decision_owners': []},
                  'limits': {'max_tokens': sum(t['max_tokens'] for t in templates),
                             'max_wall_ms': sum(t['document']['limits']['wall_ms'] for t in templates) + 60000,
                             'max_dispatches': 16}}
    return {'schema': 'af/scenario-bundle@5', 'root': 'issue-workflow', 'definitions': {'issue-workflow': definition}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--af', type=Path, default=Path('af'))
    parser.add_argument('--repo', type=Path, default=Path('.'))
    parser.add_argument('--state', type=Path)
    sub = parser.add_subparsers(dest='command', required=True)
    sub.add_parser('preview')
    sub.add_parser('migration')
    p = sub.add_parser('check-plan'); p.add_argument('selection'); p.add_argument('inspection', type=Path)
    p = sub.add_parser('compose'); p.add_argument('selection'); p.add_argument('pins', nargs='+', type=Path); p.add_argument('--routing', type=Path, required=True)
    p = sub.add_parser('reconcile'); p.add_argument('manifest', type=Path)
    p = sub.add_parser('plan'); p.add_argument('selection'); p.add_argument('--file', type=Path, required=True)
    p.add_argument('--bindings', type=Path); p.add_argument('--routing', type=Path, required=True)
    args = parser.parse_args()
    if args.command == 'preview':
        result = catalog()
    elif args.command == 'check-plan':
        result = validate_plan(args.selection, read(args.inspection))
    elif args.command == 'compose':
        result = composition(args.selection, [read(p) for p in args.pins], read(args.routing))
    elif args.state is None:
        parser.error('--state is required for native inventory, planning and reconciliation')
    elif args.command == 'migration':
        result = migration(args.af, args.repo, args.state)
    elif args.command == 'reconcile':
        result = reconcile(args.af, args.repo, args.state, read(args.manifest))
    else:
        installed = toml(args.repo / '.af/task-catalog.toml')
        if installed != toml(STAGED / 'replacement-catalog.toml'):
            raise ValueError('install and review the exact replacement catalog before planning')
        route(args.selection, read(args.routing))
        if selection(args.selection)['research_pipeline']:
            raise ValueError('research/design requires native Scenario composition; plan its internal Report Task and use compose')
        task = read(args.file)
        if task.get('pipeline') != {'name': args.selection, 'fallback': 'refuse'}:
            raise ValueError('Task file must select exact Pipeline with fallback refuse')
        if task.get('continue') and selection(args.selection)['light']:
            raise ValueError('light repair requires a reconciled bounded chain; use default or a new reviewed pilot')
        extra = []
        if args.bindings:
            local = toml(args.bindings)
            if set(local) - {'schema', 'providers'} or local.get('schema') != 'af.task-bindings/1':
                raise ValueError('machine-local bindings may change only account aliases')
            if any(p not in ('codex-personal', 'pi-personal') for p in local.get('providers', {}).values()):
                raise ValueError('personal aliases required')
            extra = ['--bindings', str(args.bindings)]
        result = validate_plan(args.selection, invoke(args.af, args.repo, args.state, 'plan', '--file', str(args.file), *extra))
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, OSError, subprocess.SubprocessError) as error:
        print('issue-workflows: ' + str(error), file=sys.stderr)
        sys.exit(1)
