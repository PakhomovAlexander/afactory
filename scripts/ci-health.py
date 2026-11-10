#!/usr/bin/env python3
"""Phase 1 CI evidence: bounded offline observation, deterministic routing, no remote writes.

The installed ci_observe_v1 fixture uses this module. No model text controls identity,
recurrence, completeness or remedy allowance. Contracts are also checked by native Task
acceptance. This module has no network transport and never runs a remedy.
"""
import argparse
import copy
import datetime as dt
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import sqlite3
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
SNAPSHOT = 'af.ci-health-snapshot/1'
DIAGNOSIS = 'af.ci-diagnosis/1'
SNAPSHOT_TYPE = 'custom/CiHealthSnapshot@1'
DIAGNOSIS_TYPE = 'custom/CiDiagnosis@1'
RULE = 'af.ci-fingerprint/1'
MAX_BYTES = 65536
MAX_FAILURES = 100
MAX_REFERENCES = 20
MAX_STATE = 4096
SAFE = 9007199254740991
CATEGORIES = ('transient', 'product_bug', 'test_bug', 'ci_infra', 'unknown')
NOTIFICATION = 'af.ci-health-notification/2'
FAILURE_FIELDS = ('occurrence_id', 'signature_id', 'workflow_id', 'workflow_file', 'run_id',
                  'run_attempt', 'job_id', 'job_name', 'platform', 'head_sha', 'conclusion',
                  'log_artifacts', 'release_blocking')


def require(condition, why):
    if not condition:
        raise ValueError(why)


def loads(raw):
    def pairs(values):
        result = {}
        for key, value in values:
            require(key not in result, 'Duplicate JSON field: ' + key)
            result[key] = value
        return result
    def constant(value):
        raise ValueError('Non-finite JSON value: ' + value)
    return json.loads(raw, object_pairs_hook=pairs, parse_constant=constant)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False, allow_nan=False).encode()


def digest(value):
    return 'sha256:' + hashlib.sha256(canonical(value)).hexdigest()


def text(limit=4096):
    return {'type': 'string', 'minLength': 1, 'maxLength': limit, 'pattern': '^[^\\u0000]+$'}


def obj(properties):
    return {'type': 'object', 'additionalProperties': False,
            'required': list(properties), 'properties': properties}


def array(item, maximum, minimum=0):
    return {'type': 'array', 'minItems': minimum, 'maxItems': maximum, 'uniqueItems': True, 'items': item}


def schema_documents():
    ident = {'type': 'string', 'pattern': '^sha256:[0-9a-f]{64}$'}
    sha = {'type': 'string', 'pattern': '^[0-9a-f]{40}$'}
    timestamp = {'type': 'string', 'pattern': '^20[0-9]{2}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$'}
    positive = {'type': 'integer', 'minimum': 1, 'maximum': SAFE}
    nullable = lambda item: {'anyOf': [{'type': 'null'}, item]}
    failure = obj({
        'occurrence_id': ident, 'signature_id': ident, 'workflow_id': positive,
        'workflow_file': text(512), 'run_id': positive, 'run_attempt': positive,
        'job_id': positive, 'job_name': text(256), 'platform': text(128), 'head_sha': sha,
        'conclusion': {'enum': ['failure', 'timed_out', 'cancelled', 'startup_failure', 'action_required']},
        'normalized_fingerprint': nullable(text(2048)), 'log_artifacts': array(ident, MAX_REFERENCES),
        'first_seen_at': timestamp, 'last_seen_at': timestamp, 'occurrence_count': positive,
        'release_blocking': {'type': 'boolean'}})
    snapshot = obj({'schema': {'const': SNAPSHOT}, 'repository': {'type': 'string', 'pattern': '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$'},
                    'captured_at': timestamp, 'source_sha': sha,
                    'release_pr': nullable(obj({'number': positive, 'head_sha': sha})),
                    'nightly_receipt': nullable(obj({'identity': ident, 'conclusion': text(128)})),
                    'complete': {'type': 'boolean'}, 'omissions': array(text(256), 100),
                    'failures': array(failure, MAX_FAILURES)})
    diagnosis = obj({'schema': {'const': DIAGNOSIS}, 'snapshot_id': ident,
                     'source_artifact_ids': array(ident, 1, 1), 'occurrence_id': ident,
                     'signature_id': ident, 'source_sha': sha, 'category': {'enum': list(CATEGORIES)},
                     'rationale': text(4096), 'evidence': array(obj({'snapshot_id': ident,
                         'source_sha': sha, 'reference': text(256)}), 20, 1)})
    selection = obj({'schema': {'const': 'af.ci-diagnosis-selection/1'}, 'occurrence_id': ident,
                     'signature_id': ident, 'source_sha': sha})
    scope = obj({'workflow_id': positive, 'job_name': text(256), 'platform': text(128), 'head_sha': sha, 'last_seen_at': timestamp})
    state = obj({'schema': {'const': 'af.ci-signature-state/1'}, 'scopes': array(scope, MAX_STATE),
        'first_seen_at': timestamp, 'last_seen_at': timestamp,
        'occurrences': {'type': 'object', 'maxProperties': MAX_STATE,
            'propertyNames': ident, 'additionalProperties': obj({'at': timestamp, 'run': positive, 'attempt': positive})},
        'transient_count': {'type': 'integer', 'minimum': 0, 'maximum': SAFE},
        'repair_count': {'type': 'integer', 'minimum': 0, 'maximum': SAFE}})
    service_job = obj({'workflow_id': positive, 'workflow_file': text(512), 'run_id': positive,
        'run_attempt': positive, 'job_id': positive, 'job_name': text(256), 'platform': text(128),
        'head_sha': sha, 'conclusion': {'enum': ['success', 'failure', 'timed_out', 'cancelled', 'startup_failure', 'action_required']},
        'failing_tests': array(text(2042), 100), 'error_lines': {'type': 'array', 'maxItems': 100, 'items': text(4096)},
        'log_artifacts': array(ident, 20), 'release_blocking': {'type': 'boolean'}})
    service = obj({'schema': {'const': 'af.ci-observe-service/1'}, 'repository': snapshot['properties']['repository'],
        'captured_at': timestamp, 'source_sha': sha, 'release_pr': snapshot['properties']['release_pr'],
        'nightly_receipt': snapshot['properties']['nightly_receipt'], 'omissions': array(text(256),100),
        'jobs': {'type': 'array', 'maxItems':100, 'items': service_job}})
    notification = obj({'schema': {'const': NOTIFICATION},
        'scenario_run': text(128), 'snapshot_id': ident, 'repository': snapshot['properties']['repository'],
        'main_ref': text(128), 'release_selector': text(128), 'observation_start': timestamp,
        'observation_end': timestamp, 'notification': text(256),
        'source_sha': sha, 'phase': {'enum': ['findings', 'diagnosis', 'remedy-intent', 'outcome']},
        'failures': array(obj({key: failure['properties'][key] for key in FAILURE_FIELDS}), MAX_FAILURES),
        'release_alerts': array(ident, MAX_FAILURES),
        'occurrence_id': nullable(ident), 'signature_id': nullable(ident),
        'category': nullable({'enum': list(CATEGORIES)}), 'unselected_occurrences': array(ident,100),
        'unselected_reason': {'enum': [None, 'diagnosis_reservation_exhausted']},
        'deterministic_diagnosis': nullable(diagnosis),
        'report_only': {'const': True}, 'complete': {'type': 'boolean'}, 'omissions': array(text(256),100)})
    notification['properties'].update(row={'type': 'integer', 'minimum': 1, 'maximum': 7},
        intended={'enum': ['alert_only', 'release_alert', 'infrastructure_alert', 'rerun_intended',
            'quarantine_evidence_intended', 'repair_intended', 'report_only']})
    return {name + '-v' + str(version) + '.json': {'$schema': 'https://json-schema.org/draft/2020-12/schema',
                '$id': 'urn:af:schema:' + name + ':' + str(version),
                'title': name.replace('-', ' ').capitalize() + ' v' + str(version), **shape}
            for name, shape in [('ci-health-snapshot', snapshot), ('ci-diagnosis', diagnosis), ('ci-diagnosis-selection', selection),
                ('ci-signature-state', state), ('ci-observe-service', service), ('ci-health-notification', notification)]
            for version in [2 if name == 'ci-health-notification' else 1]}


_VALIDATOR = None


def structural(value, name):
    # Reuse the strict stdlib interpreter, compiled once per bounded observer invocation.
    global _VALIDATOR
    if _VALIDATOR is None:
        module = globals().get('_CI_SCHEMA')
        if module is None:
            spec = importlib.util.spec_from_file_location('ci_schema', ROOT / 'scripts/backlog-check-triage.py')
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
        _VALIDATOR = module.Validator()
        for document in schema_documents().values():
            _VALIDATOR.schemas[document['$id']] = document
    errors = _VALIDATOR.errors(value, 'urn:af:schema:' + name + (':2' if name == 'ci-health-notification' else ':1'))
    require(not errors, '; '.join(errors[:4]))
    require(len(canonical(value)) <= (MAX_BYTES if name in ('ci-health-snapshot', 'ci-diagnosis') else 1_048_576), 'CI evidence exceeds byte bound')


def instant(value):
    require(isinstance(value, str) and re.fullmatch(r'20\d\d-\d\d-\d\dT\d\d:\d\d:\d\dZ', value), 'Expected exact UTC timestamp')
    return dt.datetime.strptime(value, '%Y-%m-%dT%H:%M:%SZ').replace(tzinfo=dt.timezone.utc)


def occurrence_id(repository, job):
    for key in ('workflow_id', 'run_id', 'run_attempt', 'job_id'):
        require(type(job.get(key)) is int and 0 < job[key] <= SAFE, 'Missing job identity: ' + key)
    require(re.fullmatch('[0-9a-f]{40}', job.get('head_sha', '')) is not None, 'Missing head SHA')
    return digest([repository, job['workflow_id'], job['run_id'], job['run_attempt'], job['job_id'], job['head_sha']])


def normalize(line):
    """Pinned af.ci-fingerprint/1 rules. Preserve assertion text and test/platform names."""
    line = re.sub(r'\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[@-_])', '', line)
    line = re.sub(r'\b\d{4}-\d\d-\d\d[T ][0-9:.]+(?:Z|[+-]\d\d:\d\d)?\s*', '', line)
    line = re.sub(r'\b\d\d:\d\d:\d\d(?:\.\d+)?\s*', '', line)
    line = re.sub(r'(?<![A-Za-z0-9_])\d+(?:\.\d+)?\s*(?:ns|µs|us|ms|seconds?|secs?|minutes?|mins?|hours?|hrs?|s|m|h)\b', '<duration>', line)
    line = re.sub(r'(?:/private)?/(?:tmp|var/tmp)/[^\s:]+|[A-Za-z]:\\(?:Temp|Users\\[^\\]+\\AppData\\Local\\Temp)\\[^\s:]+', '<tmp>', line)
    line = re.sub(r'(?:/private)?/var/folders/[^/\s]+/[^/\s]+/[TC]/[^\s:]+|/home/runner/work/_temp/[^\s:]+', '<tmp>', line)
    line = re.sub(r'\b[0-9a-f]{40,64}\b', '<sha>', line, flags=re.IGNORECASE)
    line = re.sub(r'\b(sha|commit)[ #:=]+[0-9a-f]{7,64}\b', r'\1 <sha>', line, flags=re.IGNORECASE)
    line = re.sub(r'\b(run|job)(?:[_ -]?(?:id|number))?[ #:=]+\d+\b', r'\1 <id>', line, flags=re.IGNORECASE)
    return line.rstrip()


def fingerprint(job):
    tests = job.get('failing_tests', [])
    require(isinstance(tests, list) and len(tests) <= 100, 'Unbounded failing tests')
    if tests:
        require(all(isinstance(t, str) and t.strip() and '\n' not in t and '\r' not in t and 0 < len(t) <= 2042 for t in tests), 'Malformed test identity')
        # Provider order is not authority. Lexical order defines the first failing identity.
        return 'test: ' + sorted(set(tests))[0]
    lines = job.get('error_lines', [])
    require(isinstance(lines, list) and len(lines) <= 100 and all(isinstance(x, str) and len(x) <= 4096 for x in lines), 'Unbounded error lines')
    return next((normalized for line in lines if (normalized := normalize(line)).strip()), None)


def signature_id(repository, job, occurrence, normalized):
    if normalized is None:
        return digest([RULE, repository, 'occurrence-only', occurrence])
    for key in ('workflow_file', 'job_name', 'platform'):
        require(isinstance(job.get(key), str) and job[key].strip(), 'Missing signature identity: ' + key)
    return digest([RULE, repository, job['workflow_file'], job['job_name'], job['platform'], normalized])


def validate_snapshot(value):
    structural(value, 'ci-health-snapshot')
    instant(value['captured_at'])
    require(not value['complete'] or value['nightly_receipt'] is not None, 'Complete snapshot lacks nightly receipt')
    require(value['complete'] == (not value['omissions']), 'Completeness must name omissions')
    seen = set()
    for failure in value['failures']:
        require(failure['occurrence_id'] == occurrence_id(value['repository'], failure), 'Occurrence identity mismatch')
        require(failure['signature_id'] == signature_id(value['repository'], failure, failure['occurrence_id'], failure['normalized_fingerprint']), 'Signature identity mismatch')
        require(failure['occurrence_id'] not in seen, 'Duplicate occurrence')
        seen.add(failure['occurrence_id'])
        require(instant(failure['first_seen_at']) <= instant(failure['last_seen_at']) <= instant(value['captured_at']), 'Invalid observation interval')
    return value


def validate_diagnosis(value, snapshot_id, snapshot):
    structural(value, 'ci-diagnosis')
    validate_snapshot(snapshot)
    require(value['snapshot_id'] == snapshot_id and value['source_artifact_ids'] == [snapshot_id], 'Diagnosis snapshot mismatch')
    failure = next((f for f in snapshot['failures'] if f['occurrence_id'] == value['occurrence_id']), None)
    require(failure is not None, 'Diagnosis occurrence absent from snapshot')
    require(value['signature_id'] == failure['signature_id'] and value['source_sha'] == failure['head_sha'], 'Diagnosis identity mismatch')
    references = {'snapshot', *failure['log_artifacts']}
    require(all(e['snapshot_id'] == snapshot_id and e['source_sha'] == failure['head_sha'] and e['reference'] in references for e in value['evidence']), 'Diagnosis evidence outside selected snapshot occurrence')
    require(failure['normalized_fingerprint'] is not None or value['category'] == 'unknown', 'Unknown signature cannot acquire a model category')
    return value


def malformed(snapshot_id, snapshot, failure, why):
    """Deterministic record, never a model's prose fallback."""
    return {'schema': DIAGNOSIS, 'snapshot_id': snapshot_id, 'source_artifact_ids': [snapshot_id],
            'occurrence_id': failure['occurrence_id'], 'signature_id': failure['signature_id'],
            'source_sha': failure['head_sha'], 'category': 'unknown',
            'rationale': 'malformed evidence: ' + why[:1024],
            'evidence': [{'snapshot_id': snapshot_id, 'source_sha': failure['head_sha'], 'reference': 'snapshot'}]}


def failure_summaries(snapshot):
    """Bounded actionable findings, retaining exact log references and release urgency."""
    return [{key: failure[key] for key in FAILURE_FIELDS} for failure in snapshot['failures']]


def diagnose(snapshot_id, snapshot, failure, task_result=None, known_transients=(), infrastructure=()):
    """Exact deterministic signatures precede the read-only pinned diagnosis Task."""
    validate_snapshot(snapshot)
    require(any(item == failure for item in snapshot['failures']), 'Selected occurrence absent from exact snapshot')
    if not snapshot['complete'] or failure['normalized_fingerprint'] is None:
        return malformed(snapshot_id, snapshot, failure, 'incomplete snapshot or missing fingerprint'), False
    for category, signatures in [('ci_infra', infrastructure), ('transient', known_transients)]:
        if failure['signature_id'] in signatures:
            result = malformed(snapshot_id, snapshot, failure, 'deterministic classification')
            result.update(category=category, rationale='Exact pinned ' + category + ' signature')
            return result, False
    try:
        require(task_result is not None, 'diagnosis Task has no admitted result')
        value = validate_diagnosis(task_result, snapshot_id, snapshot)
        require(value['occurrence_id'] == failure['occurrence_id'], 'Diagnosis belongs to a different selected occurrence')
        return value, True
    except (ValueError, TypeError, KeyError) as error:
        return malformed(snapshot_id, snapshot, failure, str(error)), True


def reviewed_gate_count(documents, inventory, failure, captured_at, evidence_platform):
    """Read-only Q2 eligibility through the existing inventory resolver and seven-day scan.

    Task-list rows carry no platform. The pin supplies the evidence platform and a mismatch
    cannot qualify a platform-specific failure. No scan --write, confirm or apply is called.
    """
    require(len(documents) <= 16 and len(canonical(documents)) <= 1_048_576
            and len(canonical(inventory)) <= 1_048_576, 'Quarantine evidence bound exceeded')
    if evidence_platform != failure['platform'] or not (failure['normalized_fingerprint'] or '').startswith('test: '):
        return 0
    spec = importlib.util.spec_from_file_location('ci_quarantine', ROOT / 'scripts/test-quarantine.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    resolve = module.resolver(inventory, module.tc.catalog())
    identity, _ = resolve(failure['normalized_fingerprint'][6:])
    if identity is None:
        return 0
    rows = []
    cutoff = int(instant(captured_at).timestamp() * 1000)
    for document in documents:
        require(document.get('schema') == 'af/task-list@4', 'Unsupported CI gate evidence list')
        for task_id, updated, flaky in module.list_rows(document, 'CI gate evidence'):
            require(len(rows) < MAX_STATE and len(flaky) <= 100, 'CI gate occurrence bound exceeded')
            if updated <= cutoff:
                selected = [name for name in flaky if resolve(name)[0] == identity]
                rows.append((task_id, updated, selected))
    facts = module.scan(rows, inventory, {'schema': 'af.test-quarantine/1', 'tests': {}}, instant(captured_at).date())
    return next((item['tasks'] for item in facts['proposed'] if item['identity'] == identity), 0)


def remedy(snapshot, failure, diagnosis, *, exhausted=False, rerun_reserved=False,
           eligible_gate_occurrences=0, small_repair=False, open_pr=False):
    """One return per ordered row. Phase 1 reports this intent; it never executes it."""
    category = diagnosis.get('category', 'unknown')
    malformed = (failure not in snapshot['failures'] or diagnosis.get('occurrence_id') != failure['occurrence_id']
                 or diagnosis.get('signature_id') != failure['signature_id'] or diagnosis.get('source_sha') != snapshot['source_sha'])
    if malformed or not snapshot['complete'] or category not in CATEGORIES or category == 'unknown' or failure['normalized_fingerprint'] is None or exhausted:
        return 1, 'alert_only'
    if failure['release_blocking']:
        return 2, 'release_alert'
    if category == 'ci_infra':
        return 3, 'infrastructure_alert'
    if category == 'transient' and not rerun_reserved:
        return 4, 'rerun_intended'
    if category == 'test_bug' and eligible_gate_occurrences >= 2:
        return 5, 'quarantine_evidence_intended'
    if category in ('product_bug', 'test_bug') and small_repair and not open_pr:
        return 6, 'repair_intended'
    return 7, 'report_only'


class Journal:
    """One bounded private sqlite state, transactional distinct-occurrence folding."""
    def __init__(self, path):
        require(not Path(path).is_symlink(), 'State symlink refused')
        self.db = sqlite3.connect(path, timeout=5)
        os.chmod(path, 0o600)
        self.db.execute('PRAGMA synchronous=FULL')
        require(self.db.execute('PRAGMA page_size').fetchone()[0] == 4096
                and self.db.execute('PRAGMA max_page_count=4096').fetchone()[0] <= 4096, 'Observer state byte bound exhausted')
        self.db.execute('CREATE TABLE IF NOT EXISTS metadata (schema TEXT NOT NULL)')
        if not self.db.execute('SELECT schema FROM metadata').fetchall():
            self.db.execute('INSERT INTO metadata VALUES (?)', ('af.ci-health-state/1',))
        require(self.db.execute('SELECT schema FROM metadata').fetchall() == [('af.ci-health-state/1',)], 'Unsupported observer state')
        self.db.execute('CREATE TABLE IF NOT EXISTS signatures (repository TEXT, signature TEXT, document TEXT NOT NULL, PRIMARY KEY(repository,signature))')
        self.db.commit()

    def close(self):
        self.db.close()

    def fold(self, repository, job, at):
        occurrence = occurrence_id(repository, job)
        normalized = fingerprint(job)
        signature = signature_id(repository, job, occurrence, normalized)
        now = instant(at)
        with self.db:
            self.db.execute('BEGIN IMMEDIATE')
            rows = self.db.execute('SELECT repository,signature,document FROM signatures').fetchall()
            require(len(rows) <= MAX_STATE, 'Observer state bound exhausted')
            for repo, sig, raw in rows:
                state = loads(raw)
                structural(state, 'ci-signature-state')
                if repo == repository:
                    require(now >= instant(state['last_seen_at']), 'Observer clock moved backwards')
                if now - instant(state['last_seen_at']) >= dt.timedelta(days=14):
                    self.db.execute('DELETE FROM signatures WHERE repository=? AND signature=?', (repo, sig))
                elif repo == repository and job['conclusion'] == 'success' and any(all(scope[key] == job[key] for key in ('workflow_id', 'job_name', 'platform', 'head_sha')) and now - instant(scope['last_seen_at']) < dt.timedelta(days=14) for scope in state['scopes']):
                    self.db.execute('DELETE FROM signatures WHERE repository=? AND signature=?', (repo, sig))
            if job['conclusion'] == 'success':
                return None
            raw = self.db.execute('SELECT document FROM signatures WHERE repository=? AND signature=?', (repository, signature)).fetchone()
            state = loads(raw[0]) if raw else {'schema': 'af.ci-signature-state/1', 'scopes': [], 'first_seen_at': at,
                'last_seen_at': at, 'occurrences': {}, 'transient_count': 0, 'repair_count': 0}
            structural(state, 'ci-signature-state')
            require(now >= instant(state['last_seen_at']), 'Observer clock moved backwards')
            # Expire individual history too, even when a signature is observed continuously.
            state['occurrences'] = {key: val for key, val in state['occurrences'].items()
                                    if now - instant(val['at']) < dt.timedelta(days=14)}
            if occurrence not in state['occurrences']:
                require(len(state['occurrences']) < MAX_STATE, 'Signature occurrence bound exhausted')
                state['occurrences'][occurrence] = {'at': at, 'run': job['run_id'], 'attempt': job['run_attempt']}
            scope = {key: job[key] for key in ('workflow_id', 'job_name', 'platform', 'head_sha')}
            state['scopes'] = [s for s in state['scopes'] if now - instant(s['last_seen_at']) < dt.timedelta(days=14)
                and not all(s[key] == scope[key] for key in scope)]
            require(len(state['scopes']) < MAX_STATE, 'Signature scope bound exhausted')
            state['scopes'].append({**scope, 'last_seen_at': at})
            state.update(first_seen_at=min(v['at'] for v in state['occurrences'].values()), last_seen_at=at)
            structural(state, 'ci-signature-state')
            if raw is None:
                require(self.db.execute('SELECT count(*) FROM signatures').fetchone()[0] < MAX_STATE, 'Observer signature bound exhausted')
            self.db.execute('INSERT OR REPLACE INTO signatures VALUES (?,?,?)', (repository, signature, canonical(state).decode()))
            count = len({(v['run'], v['attempt']) for v in state['occurrences'].values()})
            return {'occurrence_id': occurrence, 'signature_id': signature, 'normalized_fingerprint': normalized,
                    'first_seen_at': state['first_seen_at'], 'last_seen_at': at, 'occurrence_count': count}


class OfflineGithub:
    """Exact read-only routes, request/response caps and an inspectable service transcript."""
    def __init__(self, repository, routes, max_requests=16):
        require(re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository) is not None, 'Invalid repository')
        require(type(max_requests) is int and 0 < max_requests <= 32, 'Invalid GitHub request bound')
        self.repository, self.routes, self.maximum, self.requests = repository, routes, max_requests, []

    def get(self, route):
        require(route.startswith('repos/' + self.repository + '/') and '..' not in route, 'GitHub read escaped repository')
        require(len(self.requests) < self.maximum, 'GitHub request bound exhausted')
        self.requests.append(('GET', route))
        require(route in self.routes, 'Unreadable GitHub route: ' + route)
        value = self.routes[route]
        require(len(canonical(value)) <= 1_048_576, 'GitHub response byte bound exceeded')
        return copy.deepcopy(value)


def collect(github, *, captured_at, main_ref, since, nightly_receipt, release_pr=None):
    """ci_observe_v1 read path. One bounded main capture and optional exact release PR.

    OfflineGithub is the Phase 1 hermetic service transport; no live credential or service
    installation is provided. All requests are GET and evidence is capped before diagnosis.
    """
    require(isinstance(main_ref, str) and re.fullmatch(r'[A-Za-z0-9_./-]{1,128}', main_ref)
            and '..' not in main_ref, 'Invalid main ref')
    require(instant(since) <= instant(captured_at), 'Invalid observation window')
    prefix = 'repos/' + github.repository + '/'
    commit = github.get(prefix + 'commits/' + main_ref)
    require(re.fullmatch('[0-9a-f]{40}', commit.get('sha', '')) is not None, 'Missing source SHA')
    result = {'schema': 'af.ci-observe-service/1', 'repository': github.repository,
        'captured_at': captured_at, 'source_sha': commit['sha'], 'release_pr': None,
        'nightly_receipt': nightly_receipt, 'omissions': [], 'jobs': []}
    heads = [(commit['sha'], False)]
    if release_pr is not None:
        require(type(release_pr) is int and 0 < release_pr <= SAFE, 'Invalid release PR selector')
        try:
            pr = github.get(prefix + 'pulls/' + str(release_pr))
            require(pr['number'] == release_pr and pr['state'] == 'open'
                and re.fullmatch('[0-9a-f]{40}', pr['head']['sha']) is not None, 'Malformed release PR head')
            result['release_pr'] = {'number': release_pr, 'head_sha': pr['head']['sha']}
            heads.append((pr['head']['sha'], True))
        except (ValueError, KeyError, TypeError) as error:
            result['omissions'].append(str(error)[:256])
    seen = set()
    for sha, blocking in heads:
        try:
            runs = github.get(prefix + 'actions/runs?head_sha=' + sha + '&per_page=100')
            require(type(runs['total_count']) is int and 0 < runs['total_count'] <= 100, 'Run observation absent or bound exceeded')
            require(len(runs['workflow_runs']) == runs['total_count'], 'Run response incomplete')
            for run in runs['workflow_runs']:
                require(run['head_sha'] == sha and type(run['run_attempt']) is int and run['run_attempt'] > 0, 'Missing or mismatched run attempt/SHA')
                instant(run['created_at'])
                if not since <= run['created_at'] <= captured_at:
                    continue
                require(run['status'] == 'completed', 'Run evidence is still incomplete')
                jobs = github.get(prefix + 'actions/runs/' + str(run['id']) + '/attempts/' + str(run['run_attempt']) + '/jobs?per_page=100')
                require(type(jobs['total_count']) is int and 0 < jobs['total_count'] <= 100 and len(jobs['jobs']) == jobs['total_count'],
                        'Job observation bound exceeded')
                for job in jobs['jobs']:
                    observation = {'workflow_id': run['workflow_id'], 'workflow_file': run['path'].split('@', 1)[0],
                        'run_id': run['id'], 'run_attempt': run['run_attempt'], 'job_id': job['id'],
                        'job_name': job['name'], 'platform': job['platform'], 'head_sha': sha,
                        'conclusion': job['conclusion'], 'failing_tests': job.get('failing_tests', []),
                        'error_lines': job.get('error_lines', []), 'log_artifacts': job.get('log_artifacts', []),
                        'release_blocking': blocking}
                    ident = occurrence_id(github.repository, observation)
                    if ident in seen:
                        # Main and release may share a head: release blocking always dominates.
                        if blocking:
                            next(j for j in result['jobs'] if occurrence_id(github.repository, j) == ident)['release_blocking'] = True
                        continue
                    seen.add(ident)
                    require(len(result['jobs']) < 100, 'Total job observation bound exceeded')
                    result['jobs'].append(observation)
        except (ValueError, KeyError, TypeError) as error:
            result['omissions'].append(str(error)[:256])
    return result


def observe(service, journal):
    """Offline adapter/service contract: one exact repository/ref/window capture, capped input.

    A missing SHA is refused: it cannot be represented as a healthy or transient snapshot.
    Missing job details become named omissions in an incomplete snapshot.
    """
    require(len(canonical(service)) <= 1_048_576, 'Observation service response exceeds 1 MiB')
    require(service.get('schema') == 'af.ci-observe-service/1', 'Unsupported observation service')
    repository = service['repository']
    at = service['captured_at']
    instant(at)
    require(re.fullmatch('[0-9a-f]{40}', service.get('source_sha', '')) is not None, 'Missing source SHA')
    omissions = service.get('omissions', [])
    require(isinstance(omissions, list), 'Unreadable observation omissions')
    omissions = list(omissions)
    if service.get('nightly_receipt') is None:
        omissions.append('nightly receipt unavailable')
    jobs = service.get('jobs')
    if not isinstance(jobs, list):
        omissions.append('jobs unavailable')
        jobs = []
    if len(jobs) > MAX_FAILURES:
        omissions.append('job observation bound exceeded')
        jobs = jobs[:MAX_FAILURES]
    snapshot = {'schema': SNAPSHOT, 'repository': repository, 'captured_at': at,
        'source_sha': service['source_sha'], 'release_pr': service.get('release_pr'),
        'nightly_receipt': service.get('nightly_receipt'), 'complete': True, 'omissions': [], 'failures': []}
    # Malformed enclosing evidence must not reset recurrence before the final refusal.
    # Missing receipts remain representable as named omissions below.
    structural({**snapshot, 'omissions': omissions[:100]}, 'ci-health-snapshot')
    seen = {}
    def order(item):
        index, job = item
        if not isinstance(job, dict):
            return (0, 0, 0, index)
        keys = [job.get(k, 0) for k in ('run_id', 'run_attempt', 'job_id')]
        return (*[v if type(v) is int else 0 for v in keys], index)
    for index, job in sorted(enumerate(jobs), key=order):
        try:
            # Validate before touching recurrence state, including green closure evidence.
            require(isinstance(job, dict), 'Job evidence must be an object')
            occurrence = occurrence_id(repository, job)
            require(job.get('conclusion') in ('success', 'failure', 'timed_out', 'cancelled', 'startup_failure', 'action_required'), 'Missing conclusion')
            for key in ('workflow_file', 'job_name', 'platform'):
                require(isinstance(job.get(key), str) and job[key].strip(), 'Missing ' + key)
            require(type(job.get('release_blocking', False)) is bool, 'Malformed release blocking flag')
            refs = job.get('log_artifacts', [])
            require(isinstance(refs, list), 'Unreadable log references')
            if len(refs) > MAX_REFERENCES:
                omissions.append('log reference bound exceeded for job ' + str(index))
                refs = refs[:MAX_REFERENCES]
            if job.get('evidence_unreadable'):
                omissions.append('unreadable evidence for job ' + str(index))
            if occurrence in seen:
                require(seen[occurrence] == job['conclusion'], 'Conflicting conclusions for one job attempt')
                continue
            seen[occurrence] = job['conclusion']
            preview = {key: job[key] for key in ('workflow_id', 'workflow_file', 'run_id', 'run_attempt', 'job_id', 'job_name', 'platform', 'head_sha', 'conclusion')}
            if job['conclusion'] != 'success':
                normalized = fingerprint(job)
                preview.update(occurrence_id=occurrence, signature_id=signature_id(repository, job, occurrence, normalized),
                    normalized_fingerprint=normalized, first_seen_at=at, last_seen_at=at, occurrence_count=1,
                    log_artifacts=list(dict.fromkeys(refs)), release_blocking=bool(job.get('release_blocking', False)))
                structural({**snapshot, 'failures': [preview]}, 'ci-health-snapshot')
            folded = journal.fold(repository, job, at)
            if folded is None:
                scope = ('workflow_id', 'job_name', 'platform', 'head_sha')
                snapshot['failures'] = [f for f in snapshot['failures'] if not all(f[key] == job[key] for key in scope)]
                continue
            failure = {key: job[key] for key in ('workflow_id', 'workflow_file', 'run_id', 'run_attempt', 'job_id', 'job_name', 'platform', 'head_sha', 'conclusion')}
            failure.update(folded, log_artifacts=list(dict.fromkeys(refs)), release_blocking=bool(job.get('release_blocking', False)))
            structural({**snapshot, 'failures': [failure]}, 'ci-health-snapshot')
            snapshot['failures'].append(failure)
        except (ValueError, KeyError, TypeError) as error:
            omissions.append('malformed job ' + str(index) + ': ' + str(error)[:160])
    if service.get('nightly_receipt', {}) is not None and service.get('nightly_receipt', {}).get('conclusion') in ('failure', 'failed') and not snapshot['failures']:
        omissions.append('nightly failure has no observed failed job')
    snapshot['failures'].sort(key=lambda f: f['occurrence_id'])
    snapshot['omissions'] = list(dict.fromkeys(omissions))[:100]
    snapshot['complete'] = not snapshot['omissions']
    while len(canonical(snapshot)) > MAX_BYTES and snapshot['failures']:
        snapshot['failures'].pop()
        snapshot['complete'] = False
        if '64 KiB evidence bound exceeded' not in snapshot['omissions']:
            snapshot['omissions'] = snapshot['omissions'][:99] + ['64 KiB evidence bound exceeded']
    return validate_snapshot(snapshot)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    schemas = sub.add_parser('schemas')
    schemas.add_argument('--write', action='store_true')
    observer = sub.add_parser('observe', help='offline, report-only observation')
    observer.add_argument('--service', required=True, type=Path)
    observer.add_argument('--state', required=True, type=Path)
    args = parser.parse_args()
    if args.command == 'schemas':
        for name, schema in schema_documents().items():
            path = ROOT / 'schemas' / name
            rendered = json.dumps(schema, indent=2, sort_keys=True) + '\n'
            if args.write:
                path.write_text(rendered)
            else:
                require(path.read_text() == rendered, 'CI health schema parity drift: ' + name)
    else:
        with args.service.open('rb') as stream:
            raw = stream.read(1_048_577)
        require(len(raw) <= 1_048_576, 'Observation input bound exceeded')
        journal = Journal(args.state)
        try:
            print(canonical(observe(loads(raw), journal)).decode())
        finally:
            journal.close()


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, sqlite3.Error) as exc:
        print(str(exc), file=sys.stderr)
        raise SystemExit(1)
