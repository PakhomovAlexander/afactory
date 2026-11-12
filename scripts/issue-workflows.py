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
from perflab import contracts as kernel_contracts

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


def validate_plan(name, value, state):
    """Compare effective native bindings with the exact staged Worker definitions and pins."""
    internal = {catalog()['verification']} | {s['research_pipeline'] for s in catalog()['selections'].values()}
    if name not in internal:
        selection(name)
    validate_native_projection(value, state)
    native = toml(STAGED / 'replacement-catalog.toml')
    plan, graph = value['plan'], value['graph']
    validate_policy(value, state)
    if value['selection']['assessment']['decision'] != {'kind': 'selected', 'pipeline': name}:
        raise ValueError('native selection differs')
    definition = toml(STAGED / 'task-packages' / name / 'pipeline.toml')
    expected_slots = {'root.slots.' + slot: declared['worker']
                      for slot, declared in definition['slots'].items()}
    if set(plan.get('bindings', {})) != set(expected_slots) or set(graph['slots']) != set(expected_slots):
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
    # Native compilation also retains the exact catalog-selected Task-kind package.
    for kind in definition['accepts']['kinds']:
        if kind in native.get('kinds', {}):
            closure(native['kinds'][kind])
    if set(plan['dependencies']) != required_packages:
        raise ValueError('missing or extra Pipeline dependencies: expected ' + repr(sorted(required_packages))
                         + '; actual ' + repr(sorted(plan['dependencies'])))
    for slot, bound in plan['bindings'].items():
        worker = graph['slots'][slot]['worker']
        if worker != expected_slots[slot]:
            raise ValueError('effective Worker differs from declared role: ' + slot)
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


def recorded(state, identity, kind=None):
    """Read the native Store's addressed bytes, checking both envelope domains.

    The CLI exposes checked inspections, not arbitrary metadata export. Follow only IDs
    from those inspections using the same CAS layout and canonicalizer as backlog-triage.
    Missing metadata is a refusal, never an asserted manifest fallback.
    """
    if not isinstance(identity, str) or not re.fullmatch(r'sha256:[0-9a-f]{64}', identity):
        raise ValueError('native evidence requires an exact artifact identity')
    hex_id = identity[7:]
    path = Path(state) / 'cas/objects' / hex_id[:2] / hex_id[2:]
    with path.open('rb') as stream:
        data = stream.read(LIMIT + 1)
    if len(data) > LIMIT:
        raise ValueError('native evidence exceeds 4 MiB')
    value = json.loads(data)
    if kind is None and kernel_contracts.kernel_blob_id(data) == identity:
        return value
    if not isinstance(value, dict) or value.get('artifact_id') != identity:
        raise ValueError('native artifact identity differs')
    content = kernel_contracts.kernel_digest(kernel_contracts.KERNEL_CONTENT_DOMAIN, value['payload'], 'payload')
    artifact = kernel_contracts.kernel_digest(kernel_contracts.KERNEL_ARTIFACT_DOMAIN,
                    {k: v for k, v in value.items() if k != 'artifact_id'}, 'envelope')
    canonical = []
    kernel_contracts._kernel_canonical(value, canonical, 'envelope')
    if (content != value.get('content_id') or artifact != identity or b''.join(canonical) != data
            or (kind is not None and value.get('type') != kind)):
        raise ValueError('native artifact bytes or type differ')
    return value['payload'] if kind is not None else value


def canonical(value):
    parts = []
    kernel_contracts._kernel_canonical(value, parts, 'inspection')
    return b''.join(parts)


def validate_native_projection(value, state):
    """Authenticate complete projections before interpreting roles or policy.

    This is evidence validation only; native exact-plan approval still owns execution.
    """
    if value.get('schema') != 'af/task-inspection@20':
        raise ValueError('unsupported native inspection')
    plan = recorded(state, value['plan_id'], 'af/ExecutionPlan@4')
    if canonical(plan) != canonical(value['plan']):
        raise ValueError('inspection plan differs from native stored plan')
    graph = recorded(state, plan['compiled_graph_id'], 'af/CompiledTask@4')
    if canonical(graph) != canonical(value['graph']):
        raise ValueError('inspection graph differs from native stored graph')
    revision = recorded(state, value['revision_id'], 'af/TaskRevision@1')
    if (plan['task_revision_id'] != value['revision_id']
            or revision['task_id'] != value['task_id']
            or canonical(revision['inputs']) != canonical(plan['inputs'])
            or canonical(revision['authority']) != canonical(plan['authority'])
            or canonical(graph['inputs']) != canonical(plan['inputs'])):
        raise ValueError('native Task/revision/plan linkage differs')


def issue_identity(document):
    # Pipeline strategy/kind describe the stage, not the issue. Both the original goal
    # and all structured business requirements/acceptance must remain byte-for-byte equal.
    return json.dumps({k: document.get(k) for k in ('goal', 'requirements', 'acceptance', 'issue')},
                      sort_keys=True, ensure_ascii=False, separators=(',', ':'))


def original_issue(goal, requirements, acceptance=None):
    """The business contract, derived from implementation fields, never a shared label."""
    if (not isinstance(goal, str) or not goal.strip() or not isinstance(requirements, dict)
            or not isinstance(requirements.get('acceptance'), list) or not requirements['acceptance']
            or any(not isinstance(item, str) or not item.strip() for item in requirements['acceptance'])):
        raise ValueError('original issue requires goal and structured requirements acceptance')
    return {'goal': goal, 'requirements': requirements, 'acceptance': acceptance}


def check_research_issue(requirements, original):
    contract = requirements.get('original_issue') if isinstance(requirements, dict) else None
    if not isinstance(contract, dict):
        raise ValueError('research/design requires exact original issue contract')
    exact(contract, ('goal', 'requirements', 'acceptance'))
    original_issue(**contract)
    # JSON equality retains types (True is not the integer 1).
    if json.dumps(contract, sort_keys=True) != json.dumps(original, sort_keys=True):
        raise ValueError('research/design original issue contract differs from implementation')


def native_original_issue(state, revision):
    goal, requirements, acceptance = json.loads(native_issue(state, revision))
    return original_issue(goal, requirements, acceptance[0]['items'] if acceptance else None)


def check_native_research_issue(state, revision, original):
    # Only addressed native Requirements count; manifest prose cannot supply the contract.
    _, requirements, _ = json.loads(native_issue(state, revision))
    check_research_issue(requirements, original)


def native_issue(state, revision):
    requirements = revision['inputs']['requirements']
    if requirements['artifact_type'] != 'af/Requirements@1' or len(requirements['artifact_ids']) != 1:
        raise ValueError('missing exact native issue requirements')
    payload = recorded(state, requirements['artifact_ids'][0], 'af/Requirements@1')
    acceptance = []
    for identity in revision['provenance']['input_artifact_ids']:
        value = recorded(state, identity)
        if value.get('type') == 'af/TaskAcceptance@1':
            acceptance.append(value['payload'])
    if len(acceptance) > 1:
        raise ValueError('ambiguous native acceptance')
    return json.dumps([revision['goal'], payload.get('specification'), acceptance],
                      sort_keys=True, ensure_ascii=False, separators=(',', ':'))


def source_origin(state, snapshot_id):
    snapshot = recorded(state, snapshot_id)
    if snapshot.get('schema') != 'af.task-snapshot/1':
        raise ValueError('missing native source Snapshot')
    origin = recorded(state, snapshot['origin_id'])
    if origin.get('schema') != 'af.task-source-origin/3':
        raise ValueError('missing native source origin')
    return snapshot, origin


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
    tasks, research = manifest['tasks'], manifest.get('research_tasks', [])
    if not isinstance(tasks, list) or not isinstance(research, list):
        raise ValueError('pilot stages must be Task lists')
    if 'verification_task' in manifest and not manifest['verification_task']:
        raise ValueError('verification Task identity must be nonempty')
    auxiliary = research + ([manifest['verification_task']] if manifest.get('verification_task') else [])
    all_tasks = tasks + auxiliary
    if (not tasks or len(tasks) > 64 or len(auxiliary) > 16 or len(set(all_tasks)) != len(all_tasks)
            or any(not isinstance(t, str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,127}', t) for t in all_tasks)):
        raise ValueError('all stage Task identities must be unique and bounded native names')
    if choice['light'] and len(tasks) > 2:
        raise ValueError('light permits one implementation and at most one bounded repair')
    if not choice['research_pipeline'] and research:
        raise ValueError('this route has no research stage')
    values, revisions = {}, {}
    for task in all_tasks:
        value = invoke(af, repo, state, 'explain', task)
        if value.get('schema') != 'af/task-inspection@20' or value['task_id'] != task:
            raise ValueError('native Task identity differs')
        revision = recorded(state, value['revision_id'], 'af/TaskRevision@1')
        if revision['task_id'] != task or revision['inputs'] != value['plan']['inputs']:
            raise ValueError('native revision/plan inputs differ')
        values[task], revisions[task] = value, revision
    original = revisions[tasks[0]]
    original_source = original['inputs']['source']['snapshot_id']
    _, origin = source_origin(state, original_source)
    roots = subprocess.run(['git', '-C', str(repo), 'rev-list', '--max-parents=0', 'HEAD'],
                           check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30)
    repository_id = ','.join(sorted(set(roots.stdout.decode().split())))
    if (origin.get('kind') != 'captured' or origin.get('source_revision') != manifest['source_commit']
            or origin.get('repository_id') != repository_id or not repository_id):
        raise ValueError('pilot source commit or repository differs from native origin')
    issue = native_issue(state, original)
    contract = native_original_issue(state, original)
    producers = set()
    for i, task in enumerate(tasks):
        value, revision = values[task], revisions[task]
        validate_plan(manifest['selection'], value, state)
        if native_issue(state, revision) != issue:
            raise ValueError('continuation changes original issue requirements')
        if i:
            previous = tasks[i - 1]
            continued = value.get('continues') or {}
            _, current_origin = source_origin(state, revision['inputs']['source']['snapshot_id'])
            bound = current_origin.get('bound_from', {})
            if (continued.get('task_id') != previous or current_origin.get('repository_id') != repository_id
                    or bound.get('snapshot_id') != continued.get('snapshot_id')
                    or bound.get('task', {}).get('task_id') != previous
                    or bound.get('task', {}).get('task_revision_id') != values[previous]['revision_id']):
                raise ValueError('pilot omits or changes cumulative continuation ancestry')
        elif value.get('continues'):
            raise ValueError('pilot must include original Task, not only last delta')
        binding = (value.get('input_bindings') or {}).get('bindings', {}).get('research_report')
        if choice['research_pipeline']:
            if not binding or binding.get('also') or not binding.get('task'):
                raise ValueError('missing exact research producer binding')
            producer = binding['task']['task_id']
            producers.add(producer)
            if producer not in research:
                raise ValueError('pilot omits required research producer and charges')
            report = values[producer]
            report_revision = revisions[producer]
            _, report_origin = source_origin(state, report_revision['inputs']['source']['snapshot_id'])
            if (report_origin != origin or report_revision['inputs']['source']['snapshot_id'] != original_source):
                raise ValueError('research source differs from original implementation')
            check_native_research_issue(state, report_revision, contract)
            check_stage(report, choice['research_pipeline'], state)
            check_output(binding, report, revision['inputs']['research_report'], 'report', 'af/Document@1')
        elif binding or 'research_report' in revision['inputs']:
            raise ValueError('this route has no research stage')
    if producers != set(research):
        raise ValueError('unrelated research Task declared')
    if manifest.get('verification_task'):
        task = manifest['verification_task']
        value, revision = values[task], revisions[task]
        check_stage(value, catalog()['verification'], state)
        if native_issue(state, revision) != issue:
            raise ValueError('final verification changes original issue requirements')
        binding = (value.get('input_bindings') or {}).get('bindings', {}).get('source', {})
        if binding.get('task', {}).get('task_id') != tasks[-1]:
            raise ValueError('final verification does not bind the final whole candidate')
        check_output(binding, values[tasks[-1]], revision['inputs']['source'], 'snapshot', 'af/SourceTree@1')
        bases = [recorded(state, identity) for identity in revision['provenance']['input_artifact_ids']]
        bases = [v['payload'] for v in bases if v.get('type') == 'af/TaskBase@1']
        if len(bases) != 1 or bases[0]['commit'] != manifest['source_commit']:
            raise ValueError('verification must retain original whole-change Base')
        _, base_origin = source_origin(state, bases[0]['snapshot_id'])
        if base_origin != origin:
            raise ValueError('verification Base source differs')
    records = [{'task_id': t, 'revision_id': values[t]['revision_id'], 'plan_id': values[t]['plan_id'],
                'chargeable_tokens': values[t]['chargeable_tokens'], 'state': values[t]['state'],
                'source_snapshot_id': revisions[t]['inputs']['source']['snapshot_id'],
                'input_bindings': values[t].get('input_bindings')} for t in all_tasks]
    return {'schema': 'af.issue-workflow-reconciliation/1', 'pilot': manifest, 'tasks': records,
            'chain_chargeable_tokens': str(sum(int(values[t]['chargeable_tokens']) for t in all_tasks)),
            'unknown_usage_attempts': sum(values[t].get('unknown_usage_attempts', 0) for t in all_tasks),
            'continuation_sources': [(values[t].get('continues') or {}).get('snapshot_id') for t in tasks],
            'final_verification_recorded': bool(manifest.get('verification_task')), 'quality_or_savings_claim': False}


def check_stage(value, pipeline, state):
    validate_plan(pipeline, value, state)
    if (value['selection']['assessment']['decision'] != {'kind': 'selected', 'pipeline': pipeline}
            or pipeline not in value['plan']['dependencies'] or value.get('continues')
            or value.get('result', {}).get('acceptance') != 'satisfied'
            or value['result'].get('execution') != 'completed'):
        raise ValueError('stage Pipeline or independent acceptance differs')


def check_output(binding, producer, input_port, port, artifact_type):
    ref = binding.get('task') or {}
    output = producer['result']['outputs'].get(port, {})
    if (binding.get('also') or ref.get('task_revision_id') != producer['revision_id']
            or ref.get('result_id') != producer['phase'].get('result_id')
            or ref.get('port') != port or ref.get('acceptance') != producer['result']['acceptance']
            or (port == 'report' and ref.get('acceptance') != 'satisfied')
            or output.get('artifact_type') != artifact_type
            or output.get('artifact_ids') != [binding.get('artifact_id')]
            or input_port.get('artifact_type') != artifact_type
            or input_port.get('artifact_ids') != [binding.get('resolved_artifact_id', binding.get('artifact_id'))]
            or (port == 'snapshot' and (not output.get('snapshot_id')
                or binding.get('snapshot_id') != output['snapshot_id']
                or input_port.get('snapshot_id') != binding.get('rerooted_snapshot_id', binding.get('snapshot_id'))))):
        raise ValueError('stage output does not match exact recorded input binding')


def approved_policy_id(kind):
    # capture_policy serializes the native typed policy before CAS hashing. These are
    # its BTreeSet fields; native tests assert parity against actual captured authority.
    policy = toml(STAGED / ('approved-' + kind + '-policy.toml'))
    if kind == 'code':
        for key in ('components', 'checks'):
            policy['rust_toolchain'][key] = sorted(policy['rust_toolchain'][key])
    else:
        policy['required_sections'] = sorted(policy['required_sections'])
    return kernel_contracts.kernel_digest(kernel_contracts.KERNEL_CONTENT_DOMAIN, policy, 'policy')


def validate_policy(value, state):
    policy_id = value['plan']['authority']['policy_id']
    revision = recorded(state, value['revision_id'], 'af/TaskRevision@1')
    if revision['authority']['policy_id'] != policy_id:
        raise ValueError('effective policy authority differs from native revision')
    authority = recorded(state, policy_id)
    pipeline = value['selection']['assessment']['decision']['pipeline']
    kind = 'report' if pipeline in ('issue-internal/report', 'issue-internal/design') else 'code'
    expected = approved_policy_id(kind)
    if authority.get(kind + '_policy_id') != expected:
        raise ValueError('effective approved ' + kind + ' policy identity differs')
    recorded(state, expected)  # Require the exact native policy bytes, not only an asserted ID.
    if kind == 'code' and authority.get('review_policy_id'):
        review = recorded(state, authority['review_policy_id'])
        if review['check_policy_id'] != expected:
            raise ValueError('effective Review check policy identity differs')
    for node in value['graph']['nodes'].values():
        operator = node['operator'].get('operator', {})
        if operator.get('remote_checks'):
            raise ValueError('approved checks must run locally')


def installed_authority(repo):
    if toml(Path(repo) / '.af/task-catalog.toml') != toml(STAGED / 'replacement-catalog.toml'):
        raise ValueError('install and review the exact replacement catalog before planning')
    if (Path(repo) / '.af/af.toml').read_bytes() != (STAGED / 'replacement-af.toml').read_bytes():
        raise ValueError('installed approved af.toml settings bytes differ')
    for kind in ('code', 'report'):
        if (Path(repo) / ('.af/' + kind + '-policy.toml')).read_bytes() != (STAGED / ('approved-' + kind + '-policy.toml')).read_bytes():
            raise ValueError('installed approved ' + kind + ' policy bytes differ')


def captured_catalog(template):
    commit = template['authority_commit']
    if not re.fullmatch(r'[0-9a-f]{40}', commit):
        raise ValueError('template requires exact source commit')
    files = {'af.toml': 'replacement-af.toml',
             'task-catalog.toml': 'replacement-catalog.toml',
             'code-policy.toml': 'approved-code-policy.toml',
             'report-policy.toml': 'approved-report-policy.toml'}
    for installed, staged in files.items():
        result = subprocess.run(['git', '-C', template['repo'], 'show', commit + ':.af/' + installed],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30, check=True)
        if len(result.stdout) > LIMIT or result.stdout != (STAGED / staged).read_bytes():
            raise ValueError('pinned source does not install approved authority: ' + installed)


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
    if issue_identity(templates[-2]['document']) != issue_identity(templates[-1]['document']):
        raise ValueError('verification must preserve exact original issue goal, requirements and acceptance')
    implementation = templates[-2]['document']
    contract = original_issue(implementation.get('goal'), implementation.get('requirements'),
                              implementation.get('acceptance'))
    if research:
        check_research_issue(templates[0]['document'].get('requirements'), contract)
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
        if args.state is None:
            parser.error('--state is required to check captured native policy authority')
        result = validate_plan(args.selection, read(args.inspection), args.state)
    elif args.command == 'compose':
        result = composition(args.selection, [read(p) for p in args.pins], read(args.routing))
    elif args.state is None:
        parser.error('--state is required for native inventory, planning and reconciliation')
    elif args.command == 'migration':
        result = migration(args.af, args.repo, args.state)
    elif args.command == 'reconcile':
        result = reconcile(args.af, args.repo, args.state, read(args.manifest))
    else:
        installed_authority(args.repo)
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
        result = validate_plan(args.selection, invoke(args.af, args.repo, args.state, 'plan', '--file', str(args.file), *extra), args.state)
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, OSError, subprocess.SubprocessError) as error:
        print('issue-workflows: ' + str(error), file=sys.stderr)
        sys.exit(1)
