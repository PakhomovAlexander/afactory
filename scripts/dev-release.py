#!/usr/bin/env python3
"""Durable repository release controller. No model, installer, or product CLI calls."""
import argparse
import contextlib
import fcntl
import hashlib
import importlib.util
import io
import json
import os
import platform
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import time
import tempfile
import zipfile

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
SCHEMA = 'af.dev-release-run/2'
AUTHORIZATION = 'normal-release-pr-check-merge-and-signed-publication-v1'
DRIVER_DEADLINE = None

ALLOWED = ('Cargo.toml', 'Cargo.lock', 'CHANGELOG.md', 'scripts/release-contract.json')

RUN_REQUIRED = ('schema', 'plan', 'identity', 'authorization', 'created', 'updated',
                'receipts', 'stage', 'status')
RUN_OPTIONAL = ('candidate', 'candidate_tree', 'merged', 'published', 'pin', 'failure')
PLAN_FIELDS = ('repo', 'remote', 'source', 'version', 'branch', 'policy', 'profile',
               'executor', 'date', 'signing_readiness', 'main_policy')
STAGES = ('preflight', 'preparation', 'validation', 'branch', 'pr', 'checks', 'merge', 'publication')
RECEIPT_FIELDS = ('status', 'at', 'source', 'checks', 'planned_paths', 'paths', 'key',
                  'commit', 'url', 'number', 'next', 'checksums', 'features', 'commands')


def canonical_workflow(path, expected):
    if not isinstance(path, str):
        return False
    canonical, separator, ref = path.partition('@')
    return canonical == expected and (not separator or bool(ref.strip()))


def state_schema():
    properties = {key: {'type': 'string'} for key in (*RUN_REQUIRED, *RUN_OPTIONAL)}
    properties.update({
        'schema': {'const': SCHEMA}, 'authorization': {'enum': [None, AUTHORIZATION]},
        'created': {'type': 'integer'}, 'updated': {'type': 'integer'},
        'published': {'type': 'boolean'}, 'pin': {'type': 'object'},
        'plan': {'type': 'object', 'additionalProperties': False, 'required': list(PLAN_FIELDS),
                 'properties': {key: {'type': 'object' if key == 'policy' else 'string'}
                                for key in PLAN_FIELDS}},
        'receipts': {'type': 'object', 'additionalProperties': False,
                     'properties': {stage: {
                         'type': 'object', 'additionalProperties': False,
                         'required': ['status', 'at'],
                         'properties': {key: {} for key in RECEIPT_FIELDS}}
                         for stage in STAGES}},
    })
    return {'$schema': 'https://json-schema.org/draft/2020-12/schema',
            'title': SCHEMA, 'type': 'object', 'additionalProperties': False,
            'required': list(RUN_REQUIRED), 'properties': properties}


def validate_state(state):
    if (not isinstance(state, dict) or set(state) - set((*RUN_REQUIRED, *RUN_OPTIONAL))
            or set(RUN_REQUIRED) - set(state)):
        raise Refusal('foreign_state: unknown or missing run fields')
    if (not isinstance(state['plan'], dict) or set(state['plan']) != set(PLAN_FIELDS)
            or not isinstance(state['receipts'], dict) or set(state['receipts']) - set(STAGES)):
        raise Refusal('foreign_state: unknown plan or stage fields')
    policy = state['plan']['policy']
    if not isinstance(policy, dict) or policy.get('schema') != 'af.dev-release-policy/3':
        raise Refusal('foreign_state: unsupported release policy version')
    for receipt in state['receipts'].values():
        if (not isinstance(receipt, dict) or set(receipt) - set(RECEIPT_FIELDS)
                or not {'status', 'at'} <= set(receipt)):
            raise Refusal('foreign_state: invalid receipt fields')
    if (state['authorization'] not in (None, AUTHORIZATION)
            or not isinstance(state['created'], int) or not isinstance(state['updated'], int)
            or state['stage'] not in STAGES):
        raise Refusal('foreign_state: invalid authority/time/stage')
    if state['schema'] != SCHEMA or state['identity'] != digest(state['plan']):
        raise Refusal('foreign_state: unsupported or inconsistent identity')



class Refusal(Exception):
    pass


class DriverTimeout(Exception):
    pass


def remaining():
    if DRIVER_DEADLINE is None:
        return 14400
    seconds = DRIVER_DEADLINE - time.monotonic()
    if seconds <= 0:
        raise DriverTimeout()
    return min(14400, seconds)


def run(argv, cwd=None, accepted=(0,)):
    try:
        result = subprocess.run([str(x) for x in argv], cwd=cwd, capture_output=True, text=True,
                                env={**os.environ, 'GIT_TERMINAL_PROMPT': '0',
                                     'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_CONFIG_NOSYSTEM': '1'},
                                timeout=remaining())
    except subprocess.TimeoutExpired:
        if DRIVER_DEADLINE is not None and time.monotonic() >= DRIVER_DEADLINE:
            raise DriverTimeout() from None
        raise
    if result.returncode not in accepted:
        # Never retain transport diagnostics: credential-bearing URLs can appear there.
        operation = next((str(x) for x in argv[1:] if str(x) in
                          {'fetch', 'push', 'add', 'worktree', 'update-ref', 'commit-tree', 'api'}), str(argv[1]))
        raise Refusal('command_failed: %s %s (exit %s); inspect credentials/checks independently' %
                      (argv[0], operation, result.returncode))
    return result.stdout.strip()


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()


def save(path, state):
    data = (json.dumps(state, indent=2, sort_keys=True) + '\n').encode()
    if len(data) > 256 * 1024:
        raise Refusal('state_budget: run exceeds 256 KiB')
    temporary = path.with_suffix('.tmp')
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.fchmod(fd, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    fd = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def load(path):
    if path.is_symlink() or path.stat().st_size > 256 * 1024:
        raise Refusal('foreign_state: symlink or oversized state')
    state = json.loads(path.read_text())
    validate_state(state)
    return state


@contextlib.contextmanager
def lease(directory):
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    if directory.is_symlink() or directory.stat().st_uid != os.getuid():
        raise Refusal('state_permissions: require owned non-symlink directory')
    if directory.stat().st_mode & 0o077:
        raise Refusal('state_permissions: chmod 700 the state directory')
    if (directory / 'lock').is_symlink():
        raise Refusal('foreign_state: lock symlink refused')
    with (directory / 'lock').open('a') as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise Refusal('concurrent_release: another controller holds the repository lease') from error
        yield


class Controller:
    def __init__(self, args, directory):
        self.args = args
        self.directory = directory
        self.path = directory / 'run.json'
        self.mirror = directory / 'repository.git'
        self.tree = directory / 'candidate'
        self.state = None

    def git(self, *args):
        return run(['git', '-c', 'credential.helper=', '-c', 'credential.helper=!gh auth git-credential',
                    '--git-dir', self.mirror, *args])

    def gh(self, *args):
        return run(['gh', *args, '--repo', self.args.repo])

    def api(self, endpoint):
        return json.loads(run(['gh', 'api', 'repos/' + self.args.repo + '/' + endpoint]))

    def fetch(self):
        self.git('fetch', '--quiet', '--no-tags', 'origin', '+refs/heads/main:refs/heads/main',
                 '+refs/tags/*:refs/tags/*')
        return self.git('rev-parse', 'refs/heads/main')

    def blob(self, source, path):
        return self.git('show', source + ':' + path)

    def receipt(self, stage, status, **details):
        self.state['stage'] = stage
        self.state['status'] = status
        self.state['updated'] = int(time.time())
        self.state['receipts'][stage] = {'status': status, 'at': int(time.time()), **details}
        save(self.path, self.state)
        print('release: %s %s; elapsed=%ss; %s' % (stage, status,
              int(time.time()) - self.state['created'], details.get('next', details.get('url', ''))), file=sys.stderr)

    def remote_branch(self):
        rows = self.git('ls-remote', 'origin', 'refs/heads/' + self.state['plan']['branch']).split()
        return rows[0] if rows else None

    def preflight(self, source):
        policy = json.loads((ROOT / 'scripts/release-policy.json').read_text())
        for name in ('dev', 'scripts/dev-release.py', 'scripts/release-policy.json', 'rust-toolchain.toml'):
            if self.blob(source, name) != (ROOT / name).read_text().strip():
                raise Refusal('authority_source_mismatch: fetched source must contain reviewed ' + name)
        for path, tokens in policy['required_features'].items():
            content = self.blob(source, path)
            if any(token not in content for token in tokens):
                raise Refusal('source_inclusion: missing required feature ' + path)
        if not policy['compatibility'].strip():
            raise Refusal('compatibility_metadata: review scripts/release-policy.json')
        if policy.get('schema') != 'af.dev-release-policy/3':
            raise Refusal('policy_version: unsupported policy')
        planned = (*ALLOWED, 'changelog.d', 'fixtures/performance/kernel-measure')
        for row in self.git('ls-tree', '-r', source).splitlines():
            metadata, path = row.split('\t', 1)
            if any(path == target or path.startswith(target + '/') or target.startswith(path + '/')
                   for target in planned):
                if metadata.split()[0] not in ('100644', '100755'):
                    raise Refusal('planned_path: symlink/submodule in metadata path ' + path)
        for path in ALLOWED:
            self.blob(source, path)
        repository = self.api('')
        if repository['full_name'] != self.args.repo or not repository.get('permissions', {}).get('push'):
            raise Refusal('repository_permission: Contents and Pull requests write required')
        # Presence is observable; secret values are never read or retained.
        secrets = self.api('actions/secrets').get('secrets', [])
        names = {item['name'] for item in secrets}
        self.pin_readiness = 'PENDING' if 'RELEASE_PIN_TOKEN' in names else 'CREDENTIAL_MISSING'
        missing = {'MINISIGN_SECRET_KEY'} - names
        if missing:
            raise Refusal('runner_configuration: missing observable secret names ' + ', '.join(sorted(missing)))
        self.merge_prerequisite()
        self.validation_policy(source, policy)
        labels = self.api('labels?per_page=100')
        profile = self.state['plan']['profile'] if self.state else (self.args.profile or 'fast')
        if 'release:' + profile not in {item['name'] for item in labels}:
            raise Refusal('release_label: provision release:' + profile + ' with make test-labels APPLY=1')
        return policy

    def pages(self, endpoint, key=None):
        rows = []
        total = None
        for page in range(1, 11):
            data = self.api(endpoint + ('&' if '?' in endpoint else '?') +
                            'per_page=100&page=' + str(page))
            batch = data[key] if key else data
            if not isinstance(batch, list):
                raise Refusal('unobservable_policy: invalid API collection')
            if key:
                count = data.get('total_count')
                if not isinstance(count, int) or count < 0 or (total is not None and total != count):
                    raise Refusal('incomplete_observation: missing or changing API total_count')
                total = count
            rows.extend(batch)
            if len(batch) < 100:
                if total is not None:
                    if len(rows) != total:
                        raise Refusal('incomplete_observation: partial API collection')
                    ids = [row.get('id') for row in rows]
                    if any(not isinstance(i, int) for i in ids) or len(ids) != len(set(ids)):
                        raise Refusal('incomplete_observation: ambiguous API collection identities')
                return rows
        raise Refusal('observation_budget: API collection exceeds 1000 entries')

    def merge_prerequisite(self):
        rules = self.pages('rules/branches/main')
        if not any(r.get('type') == 'required_status_checks'
                   and r.get('parameters', {}).get('strict_required_status_checks_policy') is True
                   and r['parameters'].get('required_status_checks') for r in rules):
            raise Refusal('expected_base_prerequisite: administrator must configure an active main '
                          'ruleset with required status checks and Require branches to be up to date; '
                          'release actor must have no bypass. See docs/dev-release.md')
        # Effective branch rules do not describe bypass actors. Inspect their owning
        # rulesets too, refusing any bypass rather than assuming this token cannot use it.
        for rule_id in {r['ruleset_id'] for r in rules if r.get('type') == 'required_status_checks'}:
            rule = next(r for r in rules if r.get('ruleset_id') == rule_id)
            if rule.get('ruleset_source_type') != 'Repository':
                raise Refusal('expected_base_prerequisite: repository-owned observable ruleset required')
            details = self.api('rulesets/' + str(rule_id))
            if details.get('bypass_actors') != [] or details.get('enforcement') != 'active':
                raise Refusal('expected_base_prerequisite: active ruleset without bypass actors required')

    def validation_policy(self, source, policy):
        declaration = policy.get('pr_validation', {})
        expected = {'workflow', 'source_files'}
        if (set(declaration) != expected or declaration['workflow'] != 'ci.yml'
                or set(declaration['source_files']) != {'.github/workflows/ci.yml',
                    '.github/workflows/validate.yml', 'scripts/test-classes.py',
                    '.config/test-classes.json'}):
            raise Refusal('validation_policy: reviewed release PR workflow policy missing')
        for path, checksum in declaration['source_files'].items():
            if hashlib.sha256(self.blob(source, path).encode()).hexdigest() != checksum:
                raise Refusal('validation_policy: workflow semantics changed; review ' + path)
        for path in ('scripts/test-classes.py', '.config/test-classes.json'):
            if hashlib.sha256((ROOT / path).read_text().strip().encode()).hexdigest() != declaration['source_files'][path]:
                raise Refusal('authority_source_mismatch: release resolver/catalog must match fetched source')
        workflow = self.api('actions/workflows/' + declaration['workflow'])
        if (workflow.get('state') != 'active' or workflow.get('path') != '.github/workflows/ci.yml'
                or not isinstance(workflow.get('id'), int)):
            raise Refusal('validation_policy: enable the declared CI workflow')
        return workflow

    def validation_plan(self, selected):
        """Read the digest-verified resolver output of this exact run attempt."""
        artifacts = self.pages('actions/runs/%s/artifacts' % selected['id'], 'artifacts')
        name = 'validation-plan-' + str(selected['run_attempt'])
        matches = [a for a in artifacts if a.get('name') == name]
        if not matches:
            return None
        if len(matches) != 1 or matches[0].get('expired'):
            raise Refusal('checks_rejected: ambiguous or expired validation plan')
        artifact = matches[0]
        origin = artifact.get('workflow_run', {})
        if origin.get('id') != selected['id'] or origin.get('head_sha') != selected['head_sha']:
            raise Refusal('checks_rejected: foreign validation plan origin')
        archive = subprocess.run(['gh', 'api', 'repos/' + self.args.repo +
                                  '/actions/artifacts/%s/zip' % artifact['id']],
                                 capture_output=True, timeout=remaining())
        if (archive.returncode or len(archive.stdout) > 1024 * 1024
                or artifact.get('digest') != 'sha256:' + hashlib.sha256(archive.stdout).hexdigest()):
            raise Refusal('checks_rejected: validation plan download/digest differs')
        try:
            with zipfile.ZipFile(io.BytesIO(archive.stdout)) as bundle:
                if bundle.namelist() != ['plan.json'] or bundle.getinfo('plan.json').file_size > 65536:
                    raise ValueError('plan inventory/budget differs')
                plan = json.loads(bundle.read('plan.json'))
            spec = importlib.util.spec_from_file_location('release_test_classes', ROOT / 'scripts/test-classes.py')
            resolver = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(resolver)
            resolver.validate_plan(plan)
        except (ValueError, KeyError, TypeError, AttributeError, zipfile.BadZipFile) as error:
            raise Refusal('checks_rejected: invalid retained validation plan: ' + str(error)) from error
        p = self.state['plan']
        if (plan['context'] != 'release_pr' or plan['version'] != p['version']):
            raise Refusal('checks_rejected: validation plan context/version differs')
        # The event's GITHUB_SHA is the tested merge commit, while REST run/job
        # head_sha is the candidate branch SHA. Check both identities independently.
        commit = self.api('git/commits/' + plan['sha'])
        if (commit.get('sha') != plan['sha']
                or [x.get('sha') for x in commit.get('parents', [])] != [p['source'], self.state['candidate']]
                or commit.get('tree', {}).get('sha') != self.state['candidate_tree']):
            raise Refusal('checks_rejected: tested merge source/base/tree differs')
        return plan, artifact, resolver

    def latest_pr_run(self, pr, workflow):
        p = self.state['plan']
        policy = p['policy']['pr_validation']
        runs = self.pages('actions/workflows/' + policy['workflow'] +
                          '/runs?event=pull_request&head_sha=' + self.state['candidate'], 'workflow_runs')
        exact = [r for r in runs if r.get('head_sha') == self.state['candidate']
                 and r.get('workflow_id') == workflow['id']
                 and r.get('event') == 'pull_request'
                 and canonical_workflow(r.get('path'), '.github/workflows/ci.yml')
                 and r.get('head_branch') == p['branch']
                 and any(x.get('number') == pr['number']
                         and x.get('head', {}).get('sha') == self.state['candidate']
                         and x.get('base', {}).get('sha') == p['source']
                         and x.get('base', {}).get('ref') == 'main'
                         for x in r.get('pull_requests', []))]
        return max(exact, key=lambda r: (r['id'], r['run_attempt']), default=None)

    def pr_checks(self, pr):
        p = self.state['plan']
        policy = p['policy']['pr_validation']
        workflow = self.validation_policy(p['source'], p['policy'])
        selected = self.latest_pr_run(pr, workflow)
        if selected is None:
            return None
        attempt = selected['run_attempt']
        if selected['status'] != 'completed':
            return None
        evidence = self.validation_plan(selected)
        if evidence is not None:
            plan, artifact, resolver = evidence
            expected = resolver.resolve(['release:' + p['profile']], 'release_pr', plan['sha'], p['version'])
            if plan['profile'] != expected['profile'] or plan['selected'] != expected['selected']:
                return None # An older opened event may be fast, even cancelled by labeling.
        if evidence is None and selected.get('conclusion') == 'cancelled':
            return None # No retained plan establishes which profile was cancelled.
        if selected.get('conclusion') != 'success':
            raise Refusal('checks_rejected: release CI ' + str(selected.get('conclusion')))
        if evidence is None:
            return None
        jobs = self.pages('actions/runs/%s/attempts/%s/jobs' % (selected['id'], attempt), 'jobs')
        required = {'validation / resolve', 'validation / lint'}
        for os_name in ('ubuntu-latest', 'macos-latest'):
            required.update('validation / %s (%s)' % (job, os_name)
                            for job in ('base', 'check', *('class ' + c for c in plan['selected'])))
        hosted = resolver.catalog()['hosted_checks']
        required.update('validation / ' + job for job in hosted)
        names = [j['name'] for j in jobs]
        if len(names) != len(set(names)) or required != set(names):
            raise Refusal('checks_rejected: incomplete or ambiguous release CI jobs')
        needs = {job: {'result': 'success'} for job in ('resolve', 'base', 'classes')}
        needs['resolve']['outputs'] = {'plan': json.dumps(plan)}
        for job in jobs:
            # The attempt-specific endpoint is authoritative. GitHub's documented
            # job shape omits run_attempt; check it as well when supplied.
            if (job.get('head_sha') != selected['head_sha'] or job.get('run_id') != selected['id']
                    or job.get('run_attempt', attempt) != attempt):
                raise Refusal('checks_rejected: foreign CI job identity/attempt')
            local_name = job['name'].removeprefix('validation / ')
            expected = ('skipped' if local_name in hosted and hosted[local_name] not in plan['selected']
                        else 'success')
            if job.get('status') != 'completed' or job.get('conclusion') != expected:
                raise Refusal('checks_rejected: rejected or skipped release validation ' + job['name'])
            if local_name in hosted:
                needs[local_name] = {'result': job['conclusion']}
        # Reuse the same hosted exemption/aggregate contract as CI, not a separate skip list.
        resolver.aggregate(plan, needs)
        # gh exits 1 with no JSON when no checks are formally required. The
        # diagnostic is distinct from an API/permission error; never swallow either.
        result = subprocess.run(['gh', 'pr', 'checks', str(pr['number']), '--required',
                                 '--json', 'name,bucket,link', '--repo', self.args.repo],
                                capture_output=True, text=True, timeout=remaining())
        if not result.stdout.strip():
            if result.returncode == 1 and result.stderr.strip().startswith('no required checks reported on the'):
                checks = []
            else:
                raise Refusal('required_status_observation: cannot read formally required checks')
        else:
            if result.returncode not in (0, 1, 8):
                raise Refusal('required_status_observation: cannot read formally required checks')
            checks = json.loads(result.stdout)
        if any(c['bucket'] in ('fail', 'cancel') for c in checks):
            raise Refusal('checks_rejected: required PR checks failed')
        if any(c['bucket'] != 'pass' for c in checks):
            return None
        observed = self.api('actions/runs/' + str(selected['id']))
        if any(observed.get(k) != selected.get(k) for k in
               ('head_sha', 'workflow_id', 'path', 'event', 'head_branch', 'pull_requests',
                'run_attempt', 'status', 'conclusion')):
            return None # A rerun started while the attempt jobs were being read.
        # Reconcile the complete collection too: a different run can start while
        # this run's evidence is read without changing the selected run itself.
        if self.latest_pr_run(pr, workflow) != selected:
            return None
        return [{'workflow': policy['workflow'], 'run_id': selected['id'],
                 'attempt': attempt, 'plan': plan, 'plan_artifact': {k: artifact[k] for k in
                     ('id', 'name', 'digest')}, 'jobs': [{k: j[k] for k in
                     ('id', 'name', 'status', 'conclusion', 'head_sha', 'run_id')} for j in jobs]}, *checks]

    def source_checks(self, source):
        # Disposable read-only source validation, after lineage/path admission and before
        # metadata preparation. A dirty invocation checkout contributes no bytes.
        with tempfile.TemporaryDirectory(prefix='preflight-', dir=self.directory) as temporary:
            tree = Path(temporary) / 'source'
            self.git('worktree', 'add', '--detach', tree, source)
            try:
                for command in (
                    ['python3', '-I', 'scripts/release-contract.py'],
                    ['python3', '-I', 'scripts/performance/cli.py', 'harness-digest', '--check'],
                ):
                    run(command, tree)
            finally:
                self.git('worktree', 'remove', '--force', tree)

    def new_run(self):
        if not self.path.exists():
            return
        previous = load(self.path)
        if not previous.get('published') or previous['status'] != 'ready':
            raise Refusal('unfinished_run: reconcile the existing cut before --new')
        if previous['plan']['repo'] != self.args.repo or previous['plan']['remote'] != self.args.remote:
            raise Refusal('foreign_state: repository differs')
        history = self.directory / 'history'
        history.mkdir(exist_ok=True, mode=0o700)
        save(history / (previous['plan']['version'] + '.json'), previous)
        # Keep at most sixteen bounded terminal receipts. No unfinished receipt is pruned.
        receipts = sorted(history.glob('*.json'), key=lambda path: path.stat().st_mtime)
        for path in receipts[:-16]:
            path.unlink()
        if self.tree.exists():
            if self.tree.is_symlink() or Path(run(['git', 'rev-parse', '--git-common-dir'], self.tree)).resolve() != self.mirror.resolve():
                raise Refusal('foreign_state: refuse candidate cleanup')
            self.git('worktree', 'remove', '--force', self.tree)
        assets = self.directory / 'assets'
        if assets.is_symlink():
            raise Refusal('foreign_state: refuse asset cleanup')
        if assets.exists():
            shutil.rmtree(assets)
        self.path.unlink()

    def plan(self, authorized):
        if self.path.exists():
            self.state = load(self.path)
            p = self.state['plan']
            if p['repo'] != self.args.repo or p['remote'] != self.args.remote:
                raise Refusal('identity_conflict: repository or remote differs')
            if self.args.version and p['version'] != self.args.version:
                raise Refusal('version_conflict: resume never selects another version')
            if self.args.profile and p['profile'] != self.args.profile:
                raise Refusal('profile_conflict: resume retains captured validation policy')
            if self.args.source and p['source'] != self.args.source:
                raise Refusal('source_conflict: resume never retargets source')
            if authorized and not self.state['authorization']:
                self.state['authorization'] = AUTHORIZATION
                save(self.path, self.state)
            return
        for tool in ('git', 'gh', 'cargo', 'python3', 'minisign', 'npx'):
            if not shutil.which(tool):
                raise Refusal('required_tool: install ' + tool)
        required_rust = 'rustc ' + re.search(r'channel = "([^"]+)"', (ROOT / 'rust-toolchain.toml').read_text())[1]
        if not run(['rustc', '--version']).startswith(required_rust):
            raise Refusal('rust_toolchain: activate pinned ' + required_rust)
        run(['cargo', 'nextest', '--version'])
        if self.mirror.is_symlink():
            raise Refusal('foreign_state: mirror symlink refused')
        if not self.mirror.exists():
            run(['git', 'init', '--bare', self.mirror])
            self.git('remote', 'add', 'origin', self.args.remote)
        elif self.git('remote', 'get-url', 'origin') != self.args.remote:
            raise Refusal('foreign_state: mirror origin differs')
        if self.git('config', '--get-all', 'remote.origin.url') != self.args.remote:
            raise Refusal('foreign_state: mirror remote differs')
        source = self.fetch()
        if self.args.source and self.args.source != source:
            raise Refusal('authority_source_mismatch: --source must equal fetched main')
        policy = self.preflight(source)
        self.source_checks(source)
        manifest = self.blob(source, 'Cargo.toml')
        current = re.search(r'^version = "([^"]+)"', manifest, re.M)[1]
        base = current.split('-')[0]
        versions = set(self.git('tag', '--list').splitlines())
        releases = self.api('releases?per_page=100')
        versions.update(row['tag_name'] for row in releases)
        numbers = [int(t.rsplit('.', 1)[1]) for t in versions
                   if re.fullmatch('v' + re.escape(base) + r'-rc\.\d+', t)]
        if '-rc.' in current:
            numbers.append(int(current.rsplit('.', 1)[1]))
        version = self.args.version or (base + '-rc.' + str(max(numbers, default=0) + 1))
        if not re.fullmatch(re.escape(base) + r'-rc\.[1-9]\d*', version):
            raise Refusal('version_conflict: RC must use the current reviewed version base')
        if int(version.rsplit('.', 1)[1]) <= max(numbers, default=0):
            raise Refusal('version_conflict: version already used or older than source')
        branch = 'release/v' + version
        if self.git('ls-remote', 'origin', 'refs/heads/' + branch):
            raise Refusal('branch_collision: no durable matching reservation')
        if json.loads(self.gh('pr', 'list', '--head', branch, '--state', 'all', '--json', 'number')):
            raise Refusal('pr_collision: no durable matching reservation')
        profile = self.args.profile or 'fast'
        policy['validation'][-1][-1] = profile
        self.state = {'schema': SCHEMA, 'plan': {
            'repo': self.args.repo, 'remote': self.args.remote, 'source': source,
            'version': version, 'branch': branch, 'policy': policy,
            'profile': profile, 'executor': digest((ROOT / 'scripts/dev-release.py').read_text()),
            'date': time.strftime('%Y-%m-%d', time.gmtime()),
            'signing_readiness': 'unavailable locally; protected publication runner verifies key usability',
            'main_policy': 'require active strict required-status ruleset with no bypass; refuse main movement; verify merged tree and ancestry',
        }, 'authorization': AUTHORIZATION if authorized else None,
            'created': int(time.time()), 'receipts': {}, 'stage': 'preflight', 'status': 'planned'}
        self.state['pin'] = {'state': self.pin_readiness, 'url': None}
        self.state['identity'] = digest(self.state['plan'])
        self.receipt('preflight', 'planned', source=source, checks=['release-contract', 'harness-digest'],
                     features={path: digest(self.blob(source, path)) for path in policy['required_features']},
                     planned_paths=list(ALLOWED) + ['changelog.d/*', 'fixtures/performance/kernel-measure/*'])

    def unchanged_main(self):
        if self.fetch() != self.state['plan']['source']:
            raise Refusal('stale_main: approved source retained; create a separate reviewed plan, never rebase this run')

    def prepare(self):
        p = self.state['plan']
        if 'candidate' in self.state:
            if self.git('rev-parse', self.state['candidate'] + '^{tree}') != self.state['candidate_tree']:
                raise Refusal('candidate_conflict: recorded tree differs')
            return
        self.unchanged_main()
        # Check all intended paths and feature inclusion BEFORE materialization.
        self.preflight(p['source'])
        if self.tree.exists():
            # Before sealing, all files in this registered worktree are reconstructible.
            # Never delete an unregistered or redirected directory on restart.
            common = Path(run(['git', 'rev-parse', '--git-common-dir'], self.tree)).resolve()
            if common != self.mirror.resolve() or self.tree.is_symlink():
                raise Refusal('foreign_state: candidate is not this run worktree')
            self.git('worktree', 'remove', '--force', self.tree)
        self.receipt('preparation', 'intent', source=p['source'])
        self.git('config', 'core.hooksPath', '/dev/null')
        self.git('worktree', 'add', '--detach', self.tree, p['source'])
        manifest = self.tree / 'Cargo.toml'
        manifest.write_text(re.sub(r'^version = "[^"]+"', 'version = "' + p['version'] + '"',
                                   manifest.read_text(), count=1, flags=re.M))
        run(['cargo', 'update', '--workspace', '--offline', '--quiet'], self.tree)
        run(['python3', '-I', 'scripts/release-contract.py', '--prepare', p['source']], self.tree)
        section = ('## [%s] - %s\n\n### Authority compatibility\n\n%s\n\n### Changes\n\n'
                   '- Release of reviewed source `%s`; collected change notes follow.\n' %
                   (p['version'], p['date'], p['policy']['compatibility'], p['source']))
        run(['python3', 'scripts/changelog-notes.py', '--write', section], self.tree)
        run(['python3', '-I', 'scripts/performance/cli.py', 'harness-digest', '--write'], self.tree)
        changed = run(['git', 'diff', '--name-only', p['source']], self.tree).splitlines()
        if any(path not in ALLOWED and not path.startswith(('changelog.d/', 'fixtures/performance/kernel-measure/'))
               for path in changed):
            raise Refusal('path_guard: preparation changed undeclared paths')
        run(['git', 'add', '--', *ALLOWED, 'changelog.d', 'fixtures/performance/kernel-measure'], self.tree)
        # commit-tree has no hooks and uses pinned timestamps; crashes reproduce one ID.
        tree_id = run(['git', 'write-tree'], self.tree)
        timestamp = self.git('show', '-s', '--format=%cI', p['source'])
        environment = {**os.environ, 'GIT_AUTHOR_NAME': 'Afactory Release',
                       'GIT_AUTHOR_EMAIL': 'release@afactory.invalid',
                       'GIT_COMMITTER_NAME': 'Afactory Release',
                       'GIT_COMMITTER_EMAIL': 'release@afactory.invalid',
                       'GIT_AUTHOR_DATE': timestamp, 'GIT_COMMITTER_DATE': timestamp}
        result = subprocess.run(['git', 'commit-tree', tree_id, '-p', p['source'],
                                 '-m', 'release: v' + p['version']], cwd=self.tree,
                                env=environment, capture_output=True, text=True, check=True)
        candidate = result.stdout.strip()
        self.git('update-ref', 'refs/af-release/candidate', candidate)
        run(['git', 'reset', '--hard', candidate], self.tree)
        self.state['candidate'] = candidate
        self.state['candidate_tree'] = tree_id
        self.receipt('preparation', 'complete', paths=changed)

    def validate(self):
        p = self.state['plan']
        key = digest([self.state['candidate'], p['policy']['validation'], sys.version,
                      run(['rustc', '--version']), run(['cargo', '--version']), platform.platform(),
                      {k: os.environ.get(k) for k in ('RUSTFLAGS', 'RUSTDOCFLAGS', 'CARGO_BUILD_TARGET',
                       'CC', 'CXX', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_PROFILE_DEV_DEBUG', 'CARGO_PROFILE_TEST_DEBUG')}])
        previous = self.state['receipts'].get('validation', {})
        if (run(['git', 'status', '--porcelain'], self.tree)
                or run(['git', 'rev-parse', 'HEAD'], self.tree) != self.state['candidate']):
            raise Refusal('candidate_dirty: validation requires sealed candidate')
        if previous.get('status') == 'complete' and previous.get('key') == key:
            return
        self.receipt('validation', 'intent', key=key)
        commands = []
        for command in p['policy']['validation']:
            # Gate-owned reports/builds stay in this run's checkout-local target.
            if command[0] == 'scripts/verify.sh':
                command = ['env', 'AF_GATE_TARGET_DIR=' + str(self.tree / 'target'), *command]
            output = run(command, self.tree)
            commands.append({'argv': command, 'exit': 0, 'stdout_sha256': hashlib.sha256(output.encode()).hexdigest()})
        if run(['git', 'status', '--porcelain'], self.tree):
            raise Refusal('candidate_dirty: check modified source')
        self.receipt('validation', 'complete', key=key, commands=commands)

    def find_pr(self):
        p = self.state['plan']
        rows = json.loads(self.gh('pr', 'list', '--head', p['branch'], '--state', 'all',
                                  '--json', 'number,url,state,headRefOid,baseRefName,mergeCommit,isCrossRepository,labels'))
        if len(rows) > 1:
            raise Refusal('pr_collision: multiple PRs')
        if rows:
            row = rows[0]
            if (row['headRefOid'] != self.state['candidate'] or row['baseRefName'] != 'main'
                    or row['isCrossRepository']):
                raise Refusal('pr_collision: foreign head or base')
            selected = {label['name'] for label in row['labels']
                        if label['name'].startswith(('release:', 'test:', 'no-'))}
            if selected != {'release:' + p['profile']}:
                raise Refusal('profile_conflict: release PR labels differ from captured validation policy')
            if row['state'] == 'CLOSED':
                raise Refusal('pr_closed: operator action required')
            return row
        return None

    def execute(self):
        p = self.state['plan']
        if self.state['authorization'] != AUTHORIZATION:
            raise Refusal('authorization: invoke rc once to authorize the exact plan')
        if p['executor'] != digest((ROOT / 'scripts/dev-release.py').read_text()):
            raise Refusal('executor_changed: resume with the exact reviewed controller')
        tags = self.git('ls-remote', 'origin', 'refs/tags/v' + p['version'])
        if tags:
            prior_pr = self.find_pr() if self.state.get('candidate') else None
            if not prior_pr or prior_pr['state'] != 'MERGED':
                raise Refusal('tag_collision: reserved version was tagged before this release merged')
        self.prepare()
        self.validate()
        remote = self.remote_branch()
        if remote and remote != self.state['candidate']:
            raise Refusal('branch_collision: foreign release head')
        pr = self.find_pr()
        if not pr or pr['state'] != 'MERGED':
            self.unchanged_main()
            if not remote:
                self.receipt('branch', 'intent', commit=self.state['candidate'])
                self.git('push', 'origin', self.state['candidate'] + ':refs/heads/' + p['branch'])
                if self.remote_branch() != self.state['candidate']:
                    raise Refusal('branch_ambiguous: reconcile with resume')
            self.receipt('branch', 'complete', commit=self.state['candidate'])
            if not pr:
                self.receipt('pr', 'intent')
                self.gh('pr', 'create', '--base', 'main', '--head', p['branch'],
                        '--title', 'release: v' + p['version'], '--label', 'release:' + p['profile'],
                        '--body', 'Release identity `' + self.state['identity'] + '`; exact source `' +
                        p['source'] + '`.\n\n' + p['policy']['compatibility'] +
                        '\n\nValidation profile: ' + p['profile'] + '; platform/archive/signature checks remain mandatory. Pin adoption is separate.')
                pr = self.find_pr()
                if not pr:
                    raise Refusal('pr_ambiguous: reconcile with resume')
            self.receipt('pr', 'complete', url=pr['url'], number=pr['number'])
            checks = self.pr_checks(pr)
            if checks is None:
                self.receipt('checks', 'waiting', next='waiting for exact release CI and required checks; resume if stopped', url=pr['url'])
                return
            self.receipt('checks', 'complete', checks=checks)
            # A successful merge request can precede its observable result. Poll
            # the PR instead of submitting that acknowledged effect again.
            merge_receipt = self.state['receipts'].get('merge', {})
            if (merge_receipt.get('status') == 'waiting'
                    and merge_receipt.get('commit') == self.state['candidate']):
                self.receipt('merge', 'waiting', number=pr['number'], url=pr['url'],
                             commit=self.state['candidate'],
                             next='waiting for acknowledged merge; resume if stopped')
                return
            self.unchanged_main()
            self.merge_prerequisite()
            self.receipt('merge', 'intent', number=pr['number'])
            self.gh('pr', 'merge', str(pr['number']), '--squash', '--match-head-commit', self.state['candidate'])
            pr = self.find_pr()
            if not pr or pr['state'] != 'MERGED':
                self.receipt('merge', 'waiting', commit=self.state['candidate'],
                             next='waiting to reconcile merge; resume if stopped')
                return
        merged = pr['mergeCommit']['oid']
        self.fetch()
        if (self.git('rev-parse', merged + '^{tree}') != self.state['candidate_tree']
                or run(['git', '--git-dir', self.mirror, 'merge-base', p['source'], merged]) != p['source']):
            raise Refusal('merge_source_conflict: merged bytes/lineage differ; evidence cannot be reused')
        self.state['merged'] = merged
        self.receipt('merge', 'complete', commit=merged, url=pr['url'])
        self.artifacts()

    def publication_runs(self):
        runs = self.api('actions/workflows/release.yml/runs?head_sha=' + self.state['merged'])['workflow_runs']
        return [r for r in runs if r['head_sha'] == self.state['merged']]

    def publication_jobs_pass(self, release_run):
        jobs = self.api('actions/runs/' + str(release_run['id']) + '/jobs?per_page=100')['jobs']
        # The pin-only job is downstream of publication and has independent policy.
        release_jobs = [j for j in jobs if j['name'] != 'pin the release in .af/af.lock']
        failed = [j['name'] for j in release_jobs
                  if j.get('conclusion') in ('failure', 'cancelled', 'timed_out',
                                             'action_required', 'startup_failure', 'stale')]
        if failed:
            raise Refusal('release_workflow_failed: ' + ', '.join(failed) +
                          '; inspect release workflow run ' + str(release_run['id']))
        published = any(j['name'] == 'sign and publish' and j.get('conclusion') == 'success'
                        for j in release_jobs)
        if release_run.get('status') == 'completed' and not published:
            raise Refusal('release_workflow_failed: completed run ' + str(release_run['id']) +
                          ' has no successful publication join; inspect workflow prerequisites')
        return published

    def artifacts(self):
        self.state['published'] = False
        self.receipt('publication', 'reconciling')
        p = self.state['plan']
        tag = 'v' + p['version']
        runs = self.publication_runs()
        # The latest attempt owns progress. A prior failed run must not poison an
        # explicit operator rerun; signed provenance still selects its exact run below.
        publication_passed = bool(runs) and self.publication_jobs_pass(
            max(runs, key=lambda r: (r['id'], r.get('run_attempt', 1))))
        refs = self.git('ls-remote', 'origin', 'refs/tags/' + tag, 'refs/tags/' + tag + '^{}').splitlines()
        if not refs:
            self.receipt('publication', 'waiting', next='release workflow must tag/build/check/sign/publish')
            return
        commit = next((r.split()[0] for r in refs if r.endswith('^{}')), refs[0].split()[0])
        if commit != self.state['merged']:
            raise Refusal('tag_collision: release tag names another commit')
        releases = self.api('releases?per_page=100')
        release = next((r for r in releases if r['tag_name'] == tag), None)
        if release is None or release['draft']:
            self.receipt('publication', 'waiting', next='wait for protected signing/publication runner')
            return
        if not publication_passed:
            self.receipt('publication', 'waiting', next='require successful exact-source release workflow', url=release['html_url'])
            return
        names = {'af-' + tag + '-' + target + '.tar.gz' for target in p['policy']['targets']}
        names.update(('SHA256SUMS', 'SHA256SUMS.minisig', 'release-provenance.json'))
        if not names.issubset({a['name'] for a in release['assets']}):
            raise Refusal('incomplete_artifacts: required signed platform assets absent')
        assets = self.directory / 'assets'
        assets.mkdir(exist_ok=True)
        self.receipt('publication', 'intent', url=release['html_url'])
        for name in sorted(names):
            # Read back public bytes, never trust presence or API digest alone.
            self.gh('release', 'download', tag, '--pattern', name, '--dir', assets, '--clobber')
        run(['minisign', '-V', '-q', '-m', assets / 'SHA256SUMS',
             '-p', self.tree / 'crates/af/keys/release.pub'])
        sums = {}
        for line in (assets / 'SHA256SUMS').read_text().splitlines():
            checksum, name = line.split()
            if name in sums or not re.fullmatch('[0-9a-f]{64}', checksum):
                raise Refusal('checksum_format: invalid or duplicate checksum')
            sums[name] = checksum
        for name in names - {'SHA256SUMS', 'SHA256SUMS.minisig'}:
            if hashlib.sha256((assets / name).read_bytes()).hexdigest() != sums.get(name):
                raise Refusal('artifact_digest: signed checksum mismatch')
        if set(sums) != names - {'SHA256SUMS', 'SHA256SUMS.minisig'}:
            raise Refusal('checksum_inventory: signed asset set differs from expected platforms/provenance')
        provenance = json.loads((assets / 'release-provenance.json').read_text())
        if (set(provenance) != {'schema', 'version', 'source', 'repository', 'run_id'}
                or provenance['schema'] != 'af.release-provenance/1'
                or provenance['version'] != p['version']
                or provenance['source'] != self.state['merged']
                or provenance['repository'] != p['repo']):
            raise Refusal('artifact_provenance: signed version/source/repository mismatch')
        # Signed provenance names the original signing run. The successful protected
        # publication join above can restore its immutable bundle in a later run.
        if not any(r['head_sha'] == self.state['merged'] and str(r['id']) == str(provenance['run_id'])
                   for r in runs):
            self.receipt('publication', 'waiting', next='require successful exact-source release workflow', url=release['html_url'])
            return
        pins = json.loads(self.gh('pr', 'list', '--head', 'release/pin-' + tag, '--state', 'all',
                                   '--json', 'url,state'))
        self.state['published'] = True
        self.state['pin'] = pins[0] if len(pins) == 1 else self.state.get('pin', {'state': 'PENDING', 'url': None})
        self.receipt('publication', 'ready', url=release['html_url'], checksums=sums,
                     next='test RC; review pin-only PR separately')

    def drive(self):
        while True:
            remaining()
            self.state.pop('failure', None)
            self.execute()
            if self.state['status'] == 'ready' or self.args.step:
                return
            time.sleep(min(self.args.poll_interval, remaining()))

    def stopped(self, reason):
        if self.state and self.state['status'] != 'ready':
            self.state.pop('failure', None)
            stage = self.state['stage']
            # Do not erase a reusable successful receipt if the interruption
            # occurred between stages (for example just after local validation).
            while (self.state['receipts'].get(stage, {}).get('status') == 'complete'
                   and stage != STAGES[-1]):
                stage = STAGES[STAGES.index(stage) + 1]
            previous = self.state['receipts'].get(stage, {})
            details = {key: value for key, value in previous.items()
                       if key not in ('status', 'at', 'next')}
            self.receipt(stage, 'waiting', **details,
                         next=reason + '; resume this exact run to reconcile and continue')



def display(state, style):
    if style == 'json':
        print(json.dumps(state, sort_keys=True))
        return
    print('| Stage | Status | Elapsed (s) | Next action / URL |')
    print('| --- | --- | ---: | --- |')
    for stage, receipt in state['receipts'].items():
        print('| %s | %s | %s | %s |' % (stage, receipt['status'],
              max(0, receipt['at'] - state['created']), receipt.get('next', receipt.get('url', ''))))
    print('\nVersion: `%s`; source: `%s`; publication: **%s**; pin: **%s**.' %
          (state['plan']['version'], state['plan']['source'],
           'ready' if state.get('published') else state['status'], state.get('pin', {}).get('state', 'PENDING')))
    if state.get('failure'):
        print('\nFailure: ' + state['failure'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('rc', 'plan', 'status', 'resume'))
    parser.add_argument('--repo', required=True, help='explicit GitHub OWNER/REPO')
    parser.add_argument('--remote', help='credential-free Git URL; defaults to GitHub HTTPS')
    parser.add_argument('--state', type=Path, help='owned private directory outside source')
    parser.add_argument('--profile', choices=('fast', 'full'), help='validation profile (default fast)')
    parser.add_argument('--source', help='optional required exact fetched main SHA')
    parser.add_argument('--version', help='optional exact reserved RC version')
    parser.add_argument('--format', choices=('json', 'markdown'), default='markdown')
    parser.add_argument('--new', action='store_true', help='start the next run after a ready release; archive terminal receipt')
    parser.add_argument('--timeout', type=int, default=21600,
                        help='driver time limit in seconds, including commands (default 21600)')
    parser.add_argument('--poll-interval', type=int, default=30,
                        help='seconds between reconciliations, 1..300 (default 30)')
    parser.add_argument('--step', action='store_true', help='explicit single reconciliation without driving')
    parser.add_argument('--watch', action='store_true', help='read-only status refresh')
    args = parser.parse_args()
    if not 1 <= args.timeout <= 86400 or not 1 <= args.poll_interval <= 300:
        parser.error('--timeout must be 1..86400; --poll-interval must be 1..300')
    if (args.step and args.command not in ('rc', 'resume')) or (args.watch and args.command != 'status'):
        parser.error('--step requires rc/resume; --watch requires status')
    if args.new and args.command not in ('plan', 'rc'):
        parser.error('--new is only valid with plan or rc')
    if not re.fullmatch(r'[A-Za-z0-9-]+/[A-Za-z0-9_.-]+', args.repo):
        parser.error('--repo must be OWNER/REPO')
    args.remote = args.remote or 'https://github.com/' + args.repo + '.git'
    if not (args.remote == 'https://github.com/' + args.repo + '.git' or Path(args.remote).is_absolute()):
        parser.error('--remote must be matching credential-free GitHub HTTPS or an explicit local Git remote')
    directory = (args.state or Path(os.getenv('XDG_STATE_HOME', str(Path.home() / '.local/state'))) /
                 'afactory-release' / args.repo.replace('/', '--')).absolute()
    if directory.is_symlink():
        parser.error('state path must have no symlink components')
    directory = directory.resolve()
    if directory == ROOT or ROOT in directory.parents:
        parser.error('state must be outside source')
    controller = Controller(args, directory)
    def interrupt(_signum, _frame):
        raise KeyboardInterrupt()
    signal.signal(signal.SIGTERM, interrupt)
    global DRIVER_DEADLINE
    if args.command in ('rc', 'resume'):
        DRIVER_DEADLINE = time.monotonic() + args.timeout
    try:
        if args.command == 'status':
            while True:
                display(load(controller.path), args.format)
                if not args.watch:
                    return 0
                time.sleep(2)
        with lease(directory):
            try:
                if args.new:
                    controller.new_run()
                if args.command == 'resume' and not controller.path.exists():
                    raise Refusal('missing_run: plan or cut first')
                controller.plan(args.command == 'rc')
                if args.command in ('rc', 'resume'):
                    controller.drive()
                display(controller.state, args.format)
            except (DriverTimeout, KeyboardInterrupt) as error:
                reason = 'driver_timeout' if isinstance(error, DriverTimeout) else 'operator_interrupted'
                controller.stopped(reason)
                if controller.state:
                    display(controller.state, args.format)
                print('release: ' + reason + '; resume retains exact authorization', file=sys.stderr)
                return 124 if isinstance(error, DriverTimeout) else 130
        return 0
    except KeyboardInterrupt:
        return 130
    except (Refusal, OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        message = str(error) if isinstance(error, Refusal) else 'prerequisite_or_state: ' + type(error).__name__
        if controller.state:
            controller.state['failure'] = message
            controller.state['status'] = 'failed'
            save(controller.path, controller.state)
        print('release: ' + message, file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
