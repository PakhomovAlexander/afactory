#!/usr/bin/env python3
"""Inactive Phase 1 production observer: bounded repository GETs, signed logs and a nightly receipt.

The CLI in ci-health.py selects this reader explicitly. stage-adapter renders all its code,
authority and schema into one digest-pinnable executable; staging never observes or activates.
"""
from contextlib import contextmanager
import datetime as dt
import hashlib
import http.client
import importlib.util
import os
from pathlib import Path
import re
import signal
import stat
import sys
import time
import urllib.error
import urllib.request
import urllib.parse

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


C = globals().get('_CI') or module('ci_health', ROOT / 'scripts/ci-health.py')
RESPONSE_BYTES = 1_048_576
TOTAL_BYTES = 8 * RESPONSE_BYTES
READ_SECONDS = 30
WALL_SECONDS = 60
BENIGN = ('success', 'neutral', 'skipped')
FAILED = ('failure', 'timed_out', 'cancelled', 'startup_failure', 'action_required')
UNCONFIGURED_TELEGRAM = 'not sent: [publish] configures no telegram_token_file and telegram_chat_id'


class ReadUnavailable(ValueError):
    """A bounded read failed remotely; existing annotations can still supply fallback."""


class WallExpired(Exception):
    """Unwinds collection immediately; it cannot be mistaken for missing job evidence."""


def nightly():
    """Reuse the nightly's proxy-free, no-redirect, separately bounded HTTPS connector."""
    global _NIGHTLY
    if globals().get('_NIGHTLY') is None:
        sys.path.insert(0, str(ROOT / 'scripts/performance'))
        _NIGHTLY = module('ci_nightly_github', ROOT / 'scripts/nightly/nightly_github.py')
    return _NIGHTLY


def read_file(path, limit, label, private=False):
    """Bound before parsing; a FIFO, device, symlink or oversized credential is refused."""
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
        with os.fdopen(fd, 'rb') as stream:
            info = os.fstat(stream.fileno())
            C.require(stat.S_ISREG(info.st_mode) and info.st_size <= limit, label + ' byte/type bound exceeded')
            if private:
                C.require(info.st_uid in (0, os.geteuid()) and not info.st_mode & 0o077,
                          label + ' must be owner-only')
            raw = stream.read(limit + 1)
            C.require(len(raw) <= limit, label + ' byte bound exceeded')
            return raw
    except OSError as error:
        raise ValueError(label + ' unavailable') from error


def read_token(path):
    if path is None:
        return None
    try:
        token = read_file(path, 4096, 'GitHub read-only token file', private=True).decode('ascii').strip()
    except UnicodeError as error:
        raise ValueError('Malformed GitHub read-only token file') from error
    C.require(re.fullmatch(r'[A-Za-z0-9_.-]+', token) is not None, 'Malformed GitHub read-only token file')
    return token


@contextmanager
def wall_bound(seconds):
    """The executable's total wall, including a peer trickling response headers or bytes.

    The native adapter supervisor also bounds the installed process. SIGALRM is local to
    this command, never to a Worker, and is restored even after a refusal.
    """
    def expired(_number, _frame):
        raise WallExpired('CI observation wall bound exhausted')
    previous = signal.getsignal(signal.SIGALRM)
    C.require(signal.getitimer(signal.ITIMER_REAL) == (0.0, 0.0), 'Observation requires its own wall timer')
    signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, seconds)
    try:
        yield
    except WallExpired as error:
        raise ValueError('CI observation wall bound exhausted') from error
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)


class Github:
    """Repository API GETs and scoped signed logs, with shared physical request/byte caps.

    Like the nightly reader, retries one failed GET or 5xx once. Both attempts spend the
    same request/byte/wall allowance. No HTTP error body or transport diagnostic is exposed.
    """
    def __init__(self, repository, token=None, *, max_requests=16, max_bytes=TOTAL_BYTES,
                 max_wall_seconds=WALL_SECONDS, opener=None, clock=time.monotonic):
        C.require(isinstance(repository, str) and re.fullmatch(r'[A-Za-z0-9_.-]{1,100}/[A-Za-z0-9_.-]{1,100}', repository)
                  and all(part not in ('.', '..') for part in repository.split('/')), 'Invalid repository')
        C.require(type(max_requests) is int and 0 < max_requests <= 32, 'Invalid GitHub request bound')
        C.require(type(max_bytes) is int and 0 < max_bytes <= TOTAL_BYTES, 'Invalid GitHub aggregate byte bound')
        C.require(type(max_wall_seconds) is int and 0 < max_wall_seconds <= WALL_SECONDS, 'Invalid observation wall bound')
        self.repository, self.token = repository, token
        self.maximum, self.max_bytes = max_requests, max_bytes
        self.clock, self.deadline = clock, clock() + max_wall_seconds
        self.opener = opener
        self.requests, self.bytes_read = [], 0

    def remaining(self):
        left = self.deadline - self.clock()
        C.require(left > 0, 'CI observation wall bound exhausted')
        return left

    def read(self, route, *, log=False):
        prefix = 'repos/' + self.repository + '/'
        C.require(isinstance(route, str) and route.startswith(prefix) and len(route) <= 1024
                  and re.fullmatch(r'[A-Za-z0-9_./?=&-]+', route) and '..' not in route,
                  'GitHub read escaped repository')
        C.require(not log or re.fullmatch(re.escape(prefix) + r'actions/jobs/[1-9][0-9]*/logs', route),
                  'Only exact job logs may redirect')
        return self._read_url('https://api.github.com/' + route, route, log=log)

    def _read_url(self, url, label, *, log=False, download=False):
        if self.opener is None:
            self.opener = nightly().opener()
        headers = {'Accept': 'application/vnd.github+json', 'User-Agent': 'af-ci-observe-v1',
                   'X-GitHub-Api-Version': '2022-11-28'}
        if self.token and not download:
            headers['Authorization'] = 'Bearer ' + self.token
        for attempt in range(2):
            left = self.remaining()
            C.require(len(self.requests) < self.maximum, 'GitHub request bound exhausted')
            C.require(self.bytes_read < self.max_bytes, 'GitHub aggregate byte bound exhausted')
            self.requests.append(('GET', label))
            request = urllib.request.Request(url, headers=headers, method='GET')
            try:
                try:
                    response = self.opener.open(request, timeout=min(READ_SECONDS, left))
                except urllib.error.HTTPError as response_error:
                    response = response_error
                with response:
                    status = response.code if isinstance(response, urllib.error.HTTPError) else response.status
                    # Bound error bodies too, without ever printing server-controlled text.
                    limit = min(RESPONSE_BYTES, self.max_bytes - self.bytes_read)
                    chunks = bytearray()
                    while len(chunks) <= limit:
                        self.remaining()
                        chunk = response.read(min(65536, limit + 1 - len(chunks)))
                        if not chunk:
                            break
                        self.bytes_read += len(chunk)
                        chunks.extend(chunk)
                    C.require(len(chunks) <= RESPONSE_BYTES, 'GitHub response byte bound exceeded')
                    C.require(self.bytes_read <= self.max_bytes, 'GitHub aggregate byte bound exhausted')
                    self.remaining()
                    C.require(not self.token or self.token.encode() not in chunks, 'Credential-bearing GitHub response refused')
                    if status == 302 and log and not download:
                        location = response.headers.get('Location')
                        self.log_location(location)
                        # Close the API response before downloading. Never send its token.
                        break
                    if status >= 500 and attempt == 0:
                        continue
                    C.require(not 300 <= status < 400, 'GitHub redirect refused')
                    if status != 200:
                        raise ReadUnavailable('GitHub answered HTTP ' + str(status))
                    return bytes(chunks)
            except (OSError, http.client.HTTPException) as error:
                if attempt == 1:
                    raise ReadUnavailable('GitHub read failed twice') from error
        else:
            raise ValueError('GitHub read failed')
        return self._read_url(location, 'signed job log download', download=True)

    def log_location(self, location):
        """Only GitHub Actions' signed, repository-issued log URL; no redirect chain."""
        C.require(isinstance(location, str) and 0 < len(location) <= 8192
                  and location.isascii() and not any(c.isspace() or ord(c) < 32 for c in location)
                  and '\\' not in location, 'Job log download location refused')
        parsed = urllib.parse.urlsplit(location)
        # Exact Actions log-storage host families, never a generic Azure blob host.
        C.require(parsed.scheme == 'https' and parsed.username is None and parsed.password is None
                  and parsed.port is None and not parsed.fragment
                  and re.fullmatch(r'(?:productionresultssa[0-9]+\.blob\.core\.windows\.net|pipelinesactions[a-z0-9]+\.blob\.core\.windows\.net)', parsed.hostname or '')
                  and parsed.path.startswith('/') and '..' not in parsed.path and '%' not in parsed.path,
                  'Job log download location refused')
        query = urllib.parse.parse_qs(parsed.query, strict_parsing=True, keep_blank_values=True)
        C.require(all(len(values) == 1 for values in query.values())
                  and all(query.get(key, [''])[0] for key in ('sig', 'se', 'sp', 'sv', 'sr'))
                  and query['sp'] == ['r'] and query['sr'] == ['b']
                  and query.get('spr', ['https']) == ['https'], 'Unsigned job log download refused')
        C.require(not self.token or self.token not in urllib.parse.unquote(location), 'Credential-bearing job log location refused')

    def get(self, route):
        raw = self.read(route)
        try:
            value = C.loads(raw)
        except (UnicodeError, ValueError, RecursionError) as error:
            raise ValueError('Malformed GitHub JSON') from error
        C.require(isinstance(value, (dict, list)), 'Malformed GitHub document')
        return value


def positive(value):
    C.require(type(value) is int and 0 < value <= C.SAFE, 'Missing positive GitHub identity')
    return value


def page(document, field, maximum=100):
    C.require(isinstance(document, dict) and type(document.get('total_count')) is int
              and 0 <= document['total_count'] <= maximum
              and isinstance(document.get(field), list)
              and len(document[field]) == document['total_count'], 'Incomplete or oversized GitHub ' + field)
    return document[field]


def receipt(path, repository, source_sha, since, captured_at):
    """Read the actual af.nightly-publish/1 receipt, never a fixture service summary.

    A receipt of a previous green head cannot attest the current main head. Its date and
    publication must be in this observation window; stale or foreign bytes are omissions.
    """
    value = C.loads(read_file(path, RESPONSE_BYTES, 'Nightly publish receipt'))
    schema = globals().get('_RECEIPT_SCHEMA')
    if schema is None:
        schema = C.loads((ROOT / 'schemas/performance-nightly-publish-v1.json').read_bytes())
    interpreter = globals().get('_CI_SCHEMA') or module(
        'ci_receipt_schema', ROOT / 'scripts/backlog-check-triage.py')
    validator = interpreter.Validator()
    validator.schemas[schema['$id']] = schema
    C.require(not validator.errors(value, schema['$id']), 'Malformed nightly publish receipt')
    C.require(value['repository'] == repository and value['head_sha'] == source_sha, 'Nightly receipt repository/head mismatch')
    day = dt.date.fromisoformat(value['date'])
    C.require(since[:10] <= day.isoformat() <= captured_at[:10]
              and value['tag'] == 'nightly-' + day.strftime('%Y.%m.%d'), 'Nightly receipt date/tag mismatch')
    published = value['published_unix_ms']
    C.require(int(C.instant(since).timestamp() * 1000) <= published <= int(C.instant(captured_at).timestamp() * 1000),
              'Nightly receipt publication outside observation window')
    C.require((value['telegram'] is None) != (value['telegram_reason'] is None), 'Malformed nightly notification receipt')
    failed = [item['tag'] for item in value['prune_failures']]
    C.require(value['tag'] not in value['foreign'] and len(set(failed)) == len(failed)
              and not set(failed) & set(value['pruned']), 'Malformed nightly prune receipt')
    if value['status'] == 'skipped':
        C.require(value['release'] is None and not any(value[key] for key in ('assets', 'pruned', 'prune_failures', 'foreign')),
                  'Malformed skipped nightly receipt')
    else:
        names = ['af-' + value['tag'] + '-' + arch + '-unknown-linux-musl.tar.gz' for arch in ('x86_64', 'aarch64')]
        names += ['SHA256SUMS', 'SHA256SUMS.minisig', 'nightly.json', 'daily.json', 'report.html']
        C.require(value['release'] is not None and value['release']['url'] == 'https://github.com/' + repository + '/releases/tag/' + value['tag']
                  and [item['name'] for item in value['assets']] == names, 'Malformed published nightly receipt')
    notification_failed = value['telegram'] is None and value['telegram_reason'] != UNCONFIGURED_TELEGRAM
    conclusion = 'failure' if notification_failed or value['prune_failures'] or value['foreign'] else (
        'success' if value['status'] == 'published' else 'skipped')
    return {'identity': C.digest(value), 'conclusion': conclusion}


def failing_tests(lines):
    """Recognize libtest, nextest, pytest and Python unittest failure summary identities.

    Only explicit failure records count. Ignore preceding generic diagnostics and provider
    ordering; fingerprint chooses the lexical first identity under the existing contract.
    """
    patterns = (
        r'test (.+?) \.\.\. FAILED',
        r'FAIL \[[^]\r\n]{1,64}\]\s+(?:\(\s*[0-9]+/\s*[0-9]+\)\s+)?(.+)',
        r'FAILED (.+)',
        r'(?:FAIL|ERROR): ([A-Za-z_][A-Za-z0-9_]* \((?:[A-Za-z_][A-Za-z0-9_]*\.)+[A-Za-z_][A-Za-z0-9_]*\)(?: \(.+\))?)',
    )
    tests = set()
    for line in lines:
        # Strip only log decoration, never volatile-looking bytes inside a test ID.
        # Parameter values can themselves be SHAs, durations or temporary paths.
        decorated = re.sub(r'\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[@-_])', '', line).strip()
        decorated = re.sub(r'^\d{4}-\d\d-\d\d[T ][0-9:.]+(?:Z|[+-]\d\d:\d\d)?\s*', '', decorated).strip()
        for pattern in patterns:
            match = re.fullmatch(pattern, decorated)
            if match:
                identity = match[1].strip()
                if decorated.startswith('FAILED '):
                    if not re.match(r'.+\.py(?:::| - |$)', identity):
                        continue  # Generic FAILED diagnostics are not pytest node IDs.
                    # Pytest's reason separator may also occur inside parameter IDs.
                    # Recognize a balanced node ID before the first outside separator;
                    # ambiguous/unbalanced evidence fails closed instead of truncating.
                    depth = 0
                    for index, char in enumerate(identity):
                        if char == '[':
                            depth += 1
                        elif char == ']':
                            depth -= 1
                            C.require(depth >= 0, 'Ambiguous pytest identity')
                        elif depth == 0 and identity.startswith(' - ', index):
                            identity = identity[:index]
                            break
                    C.require(depth == 0 and re.fullmatch(r'.+\.py(?:::.*)?', identity), 'Ambiguous pytest identity')
                C.require(identity and len(identity) <= 2042 and '\x00' not in identity,
                          'Test identity bound exceeded')
                tests.add(identity)
                C.require(len(tests) <= 100, 'Failing test count bound exceeded')
                break
    return sorted(tests)


def collect(github, *, captured_at, main_ref, since, nightly_receipt, release_pr=None):
    C.require(isinstance(main_ref, str) and re.fullmatch(r'[A-Za-z0-9_./-]{1,128}', main_ref)
              and '..' not in main_ref, 'Invalid main ref')
    C.require(C.instant(since) <= C.instant(captured_at), 'Invalid observation window')
    if release_pr is not None:
        positive(release_pr)
    prefix = 'repos/' + github.repository + '/'
    commit = github.get(prefix + 'commits/' + main_ref)
    C.require(isinstance(commit, dict) and isinstance(commit.get('sha'), str)
              and re.fullmatch('[0-9a-f]{40}', commit['sha']), 'Missing source SHA')
    result = {'schema': 'af.ci-observe-service/1', 'repository': github.repository,
        'captured_at': captured_at, 'source_sha': commit['sha'], 'release_pr': None,
        'nightly_receipt': None, 'omissions': [], 'jobs': []}
    def omit(reason):
        if reason not in result['omissions'] and len(result['omissions']) < 100:
            result['omissions'].append(reason[:256])
    try:
        result['nightly_receipt'] = receipt(nightly_receipt, github.repository, commit['sha'], since, captured_at)
    except (ValueError, KeyError, TypeError, UnicodeError, RecursionError):
        omit('nightly publish receipt unavailable, malformed, stale or foreign')
    heads = {commit['sha']: False}
    if release_pr is not None:
        try:
            pr = github.get(prefix + 'pulls/' + str(release_pr))
            C.require(positive(pr['number']) == release_pr and pr['state'] == 'open'
                      and pr['base']['repo']['full_name'] == github.repository
                      and re.fullmatch('[0-9a-f]{40}', pr['head']['sha']), 'Malformed release PR head')
            result['release_pr'] = {'number': release_pr, 'head_sha': pr['head']['sha']}
            heads[pr['head']['sha']] = True
        except (ValueError, KeyError, TypeError):
            omit('release PR evidence unavailable or malformed')
    seen = set()
    for sha, blocking in heads.items():
        try:
            checks = page(github.get(prefix + 'commits/' + sha + '/check-runs?filter=latest&per_page=100'), 'check_runs')
            C.require(checks, 'Check-run evidence absent')
            by_id = {}
            for check in checks:
                check_id = positive(check['id'])
                if check.get('status') != 'completed' or check.get('conclusion') not in (*BENIGN, *FAILED):
                    omit('unfinished check-run evidence for job ' + str(check_id))
                    continue
                C.require(check_id not in by_id and check['head_sha'] == sha,
                          'Malformed check-run evidence')
                by_id[check_id] = check
            runs = page(github.get(prefix + 'actions/runs?head_sha=' + sha + '&per_page=100'), 'workflow_runs')
            C.require(runs, 'Run evidence absent')
            matched = set()
            for run in runs:
                run_id, attempt, workflow = [positive(run[key]) for key in ('id', 'run_attempt', 'workflow_id')]
                if run.get('status') != 'completed':
                    omit('unfinished workflow run ' + str(run_id))
                    continue
                C.require(run['head_sha'] == sha, 'Run head mismatch')
                created = C.instant(run['created_at'])
                C.require(run['repository']['full_name'] == github.repository
                          and isinstance(run['path'], str)
                          and len(run['path'].split('@', 1)[0]) <= 512
                          and re.fullmatch(r'\.github/workflows/[A-Za-z0-9_.-]+\.ya?ml', run['path'].split('@', 1)[0]),
                          'Malformed or incomplete workflow run')
                selected = github.get(prefix + f'actions/runs/{run_id}/attempts/{attempt}')
                C.require(all(positive(selected[key]) == run[key] for key in ('id', 'workflow_id', 'run_attempt'))
                          and all(selected[key] == run[key] for key in ('head_sha', 'path', 'created_at'))
                          and selected['repository']['full_name'] == github.repository
                          and selected['status'] == 'completed', 'Selected run attempt mismatch')
                started, updated = C.instant(selected['run_started_at']), C.instant(selected['updated_at'])
                C.require(created <= started <= updated, 'Malformed selected attempt interval')
                # A rerun has the original run's created_at. The selected attempt's own
                # interval determines overlap, including a run completed in this window.
                if started > C.instant(captured_at) or updated < C.instant(since):
                    continue
                C.require(updated <= C.instant(captured_at), 'Attempt completion outside capture')
                jobs = page(github.get(prefix + f'actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100'), 'jobs')
                C.require(jobs, 'Job evidence absent')
                for job in jobs:
                    try:
                        job_id = positive(job['id'])
                        C.require(positive(job['run_id']) == run_id and job['head_sha'] == sha
                                  and positive(job.get('run_attempt', attempt)) == attempt,
                                  'Job run/attempt/head mismatch')
                        C.require(job['status'] == 'completed' and job['conclusion'] in (*BENIGN, *FAILED), 'Incomplete job evidence')
                        C.require(isinstance(job['name'], str) and job['name'].strip() and len(job['name']) <= 256,
                                  'Missing or oversized job name')
                        link = 'https://api.github.com/' + prefix + 'check-runs/'
                        C.require(isinstance(job['check_run_url'], str) and job['check_run_url'].startswith(link), 'Foreign job check-run URL')
                        check_id = int(job['check_run_url'][len(link):])
                        check = by_id[check_id]
                        C.require(job['check_run_url'] == link + str(check_id) and check['name'] == job['name']
                                  and check['conclusion'] == job['conclusion'], 'Job/check evidence mismatch')
                        matched.add(check_id)
                        labels = job['labels']
                        C.require(isinstance(labels, list) and 0 < len(labels) <= 20
                                  and all(isinstance(label, str) and re.fullmatch(r'[A-Za-z0-9_. -]{1,64}', label) for label in labels),
                                  'Missing or malformed runner platform labels')
                        platform = ','.join(sorted(set(labels)))
                        C.require(len(platform) <= 128, 'Runner platform bound exceeded')
                        observation = {'workflow_id': workflow, 'workflow_file': run['path'].split('@', 1)[0],
                            'run_id': run_id, 'run_attempt': attempt, 'job_id': job_id, 'job_name': job['name'],
                            'platform': platform, 'head_sha': sha, 'conclusion': job['conclusion'],
                            'completed_at': selected['updated_at'],
                            'failing_tests': [], 'error_lines': [], 'log_artifacts': [], 'release_blocking': blocking}
                        ident = C.occurrence_id(github.repository, observation)
                        C.require(ident not in seen, 'Duplicate job attempt evidence')
                        seen.add(ident)
                        if job['conclusion'] in ('neutral', 'skipped'):
                            continue  # Neither failed evidence nor green signature closure.
                        if job['conclusion'] in FAILED:
                            try:
                                annotations = github.get(prefix + f'check-runs/{check_id}/annotations?per_page=100')
                                C.require(isinstance(annotations, list) and len(annotations) <= 100
                                          and type(check['output']['annotations_count']) is int
                                          and check['output']['annotations_count'] == len(annotations), 'Incomplete annotations')
                                for annotation in annotations:
                                    C.require(annotation['annotation_level'] in ('notice', 'warning', 'failure')
                                              and isinstance(annotation['message'], str) and len(annotation['message']) <= 4096,
                                              'Malformed check annotation')
                                    if annotation['annotation_level'] == 'failure' and annotation['message'].strip():
                                        observation['error_lines'].append(annotation['message'])
                                if annotations:
                                    observation['log_artifacts'].append(C.digest(annotations))
                                # Annotations can name only a subset of failed tests. Always
                                # combine both bounded sources before selecting lexical first.
                                observation['failing_tests'] = failing_tests(
                                    [line for message in observation['error_lines'] for line in message.splitlines()])
                                try:
                                    raw = github.read(prefix + f'actions/jobs/{job_id}/logs', log=True)
                                except ReadUnavailable:
                                    # Valid annotation fallback survives unavailable logs;
                                    # malformed, oversized or out-of-scope evidence does not.
                                    if not observation['error_lines']:
                                        raise
                                else:
                                    lines = raw.decode('utf-8').splitlines()
                                    errors = [line for line in lines if re.search(r'(?i)(?:##\[error\]|\berror\b|\bFAILED\b|\bpanicked\b|\bassertion\b)', line)]
                                    C.require(len(errors) <= 100 and all(len(line) <= 4096 for line in errors), 'Log evidence bound exceeded')
                                    observation['failing_tests'] = sorted(set(observation['failing_tests']) | set(failing_tests(lines)))
                                    C.require(len(observation['failing_tests']) <= 100, 'Failing test count bound exceeded')
                                    if not observation['error_lines']:
                                        observation['error_lines'] = errors
                                    observation['log_artifacts'].append('sha256:' + hashlib.sha256(raw).hexdigest())
                                normalized = C.fingerprint(observation)
                                C.require(normalized is not None and len(normalized) <= 2048 and '\x00' not in normalized,
                                          'No bounded deterministic failure evidence')
                                # The service needs only the line that defines the fingerprint.
                                # Full bounded reads remain named by their content digests.
                                observation['error_lines'] = next(([line] for line in observation['error_lines']
                                    if C.normalize(line).strip()), [])
                                observation['failing_tests'] = observation['failing_tests'][:1]
                            except (ValueError, KeyError, TypeError, UnicodeError):
                                omit('unreadable or malformed failure evidence for job ' + str(job_id))
                                observation.update(failing_tests=[], error_lines=[], log_artifacts=[])
                        C.require(len(result['jobs']) < 100, 'Total job observation bound exceeded')
                        result['jobs'].append(observation)
                    except (ValueError, KeyError, TypeError):
                        omit('malformed or unavailable job evidence')
            for ident, check in by_id.items():
                if check['conclusion'] not in BENIGN and ident not in matched:
                    omit('Failed check lacks exact workflow/job evidence for job ' + str(ident))
        except (ValueError, KeyError, TypeError):
            omit('incomplete or malformed checks/runs/jobs for head ' + sha)
    # API list order and run IDs are not completion order. Green closure uses only the
    # selected attempt's own completion timestamp; ties remain ambiguous and cannot erase failures.
    result['jobs'].sort(key=lambda job: (job['completed_at'], job['run_id'], job['run_attempt'], job['job_id']))
    return result


def observation_response(snapshot, request=None):
    """The exact response envelope used for capture sizing and installed observation.

    CLI captures use fixed-width native identities; an adapter invocation supplies its
    actual request. Their byte sizes agree, including the nested snapshot's escaping.
    """
    identity = C.digest(snapshot)
    return {'schema': 'af/scenario-adapter-response@1',
        'intent_id': request['intent_id'] if request else 'sha256:' + '0' * 64,
        'input_digest': request['input_digest'] if request else 'sha256:' + '0' * 64,
        'outcome': 'done', 'evidence': 'ci_observe_v1 snapshot ' + identity,
        'observation': {'subject': snapshot['repository'], 'revision': snapshot['source_sha'],
            'condition': request['inputs']['condition']['value'] if request else 'ci_observe_v1:' + identity,
            'satisfied': True, 'evidence': C.canonical(snapshot).decode()}}


def observe(options, *, opener=None, clock=time.monotonic, expected_sha=None, expected_snapshot=None,
            response_request=None):
    with wall_bound(options['max_wall_seconds']):
        github = Github(options['repository'], read_token(options.get('github_token_file')),
            max_requests=options['max_requests'], max_wall_seconds=options['max_wall_seconds'], opener=opener, clock=clock)
        service = collect(github, captured_at=options['captured_at'], main_ref=options['main_ref'],
            since=options['since'], nightly_receipt=options['nightly_receipt'], release_pr=options.get('release_pr'))
        C.require(expected_sha is None or service['source_sha'] == expected_sha, 'Exact revision precondition refused')
        github.remaining()
        journal = C.Journal(options['state'])
        # Include the executable's trailing newline in the native output cap. Bound
        # before the receipt/folds commit and before checking the selected digest.
        snapshot_fits = lambda value: len(C.canonical(observation_response(value, response_request))) + 1 <= C.MAX_BYTES
        try:
            if expected_snapshot is not None:
                # Fold against a private copy first: a refused stale snapshot must not
                # close green scopes or advance recurrence in the owner's journal.
                preview = C.Journal.__new__(C.Journal)
                preview.db = C.sqlite3.connect(':memory:')
                try:
                    journal.db.backup(preview.db)
                    C.observe(service, preview, expected_snapshot=expected_snapshot, snapshot_fits=snapshot_fits)
                finally:
                    preview.close()
            # Check again inside the commit transaction: another observer may have
            # advanced the journal after the private preview was copied.
            result = C.observe(service, journal, expected_snapshot=expected_snapshot, snapshot_fits=snapshot_fits)
        finally:
            journal.close()
        github.remaining()
        return result


@contextmanager
def occurrence_directory(path):
    root = Path(path)
    C.require(root.is_absolute() and '..' not in root.parts, 'Invalid occurrence directory')
    fd = os.open('/', os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in root.parts[1:]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = child
        info = os.fstat(fd)
        C.require(info.st_uid == os.geteuid() and not info.st_mode & 0o077, 'Occurrence directory must be owner-only')
        yield fd
    finally:
        os.close(fd)


def validate_occurrence(value):
    schema = globals().get('_OCCURRENCE_SCHEMA')
    if schema is None:
        schema = C.loads((ROOT / 'schemas/ci-observe-occurrence-v1.json').read_bytes())
    interpreter = globals().get('_CI_SCHEMA') or module('ci_occurrence_schema', ROOT / 'scripts/backlog-check-triage.py')
    validator = interpreter.Validator()
    validator.schemas[schema['$id']] = schema
    C.require(not validator.errors(value, schema['$id']), 'Malformed occurrence selectors')
    C.require(dt.timedelta(0) < C.instant(value['captured_at']) - C.instant(value['since']) <= dt.timedelta(days=1),
              'Occurrence window must be at most one day')
    C.require(Path(value['nightly_receipt']).is_absolute() and '..' not in Path(value['nightly_receipt']).parts,
              'Occurrence requires an absolute nightly receipt')
    return value


def stage_occurrence(snapshot, *, since, nightly_receipt, occurrence_dir):
    """Exclusively create one owner-approved occurrence's bounded selector document."""
    value = validate_occurrence(C.occurrence_document(snapshot, since=since, nightly_receipt=nightly_receipt))
    raw = C.canonical(value)
    C.require(len(raw) <= 8192, 'Occurrence selector byte bound exceeded')
    with occurrence_directory(occurrence_dir) as fd:
        leaf = os.open(value['snapshot_digest'][7:] + '.json', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                       0o600, dir_fd=fd)
        with os.fdopen(leaf, 'wb') as stream:
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())


def occurrence_options(options, snapshot_digest):
    """Owner-written selectors addressed by the exact Scenario snapshot content digest.

    The executable pins the directory and scope. No request supplies a path or selectors;
    each occurrence reads one bounded owner-only regular leaf, with no symlink traversal.
    """
    with occurrence_directory(options['occurrence_dir']) as fd:
        leaf = os.open(snapshot_digest[7:] + '.json', os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW, dir_fd=fd)
        try:
            info = os.fstat(leaf)
            C.require(stat.S_ISREG(info.st_mode) and info.st_uid == os.geteuid()
                      and not info.st_mode & 0o077 and info.st_size <= 8192, 'Occurrence selectors must be bounded and owner-only')
            with os.fdopen(leaf, 'rb', closefd=False) as stream:
                raw = stream.read(8193)
                C.require(len(raw) <= 8192, 'Occurrence selector byte bound exceeded')
        finally:
            os.close(leaf)
    value = validate_occurrence(C.loads(raw))
    C.require(value['snapshot_digest'] == snapshot_digest, 'Occurrence snapshot mismatch')
    return {**options, **{key: value[key] for key in ('since', 'captured_at', 'nightly_receipt', 'release_pr')}}


def adapter_response(request, options, **transport):
    """Native ci_observe_v1 protocol with an exact snapshot content precondition."""
    C.require(set(request) == {'schema', 'method', 'operation', 'destination', 'intent_id', 'input_digest', 'inputs'}
              and request['schema'] == 'af/scenario-adapter-request@4'
              and request['method'] == request['operation'] == 'observe'
              and request['destination'] == options['destination'], 'Unsupported read-only observation request')
    for key in ('intent_id', 'input_digest'):
        C.require(isinstance(request[key], str) and re.fullmatch('sha256:[0-9a-f]{64}', request[key]), 'Malformed adapter identity')
    inputs = request['inputs']
    C.require(set(inputs) == {'subject', 'revision', 'condition'}
              and all(set(v) == {'type', 'value'} and v['type'] == 'text' and isinstance(v['value'], str) for v in inputs.values())
              and inputs['subject']['value'] == options['repository']
              and re.fullmatch('ci_observe_v1:sha256:[0-9a-f]{64}', inputs['condition']['value'])
              and re.fullmatch('[0-9a-f]{40}', inputs['revision']['value']), 'Observation scope refused')
    response = {'schema': 'af/scenario-adapter-response@1', 'intent_id': request['intent_id'],
        'input_digest': request['input_digest'], 'outcome': 'failed',
        'evidence': 'ci_observe_v1 evidence unavailable or precondition refused', 'observation': None}
    try:
        expected = inputs['condition']['value'].split(':', 1)[1]
        selected = occurrence_options(options, expected)
        snapshot = observe(selected, expected_sha=inputs['revision']['value'], expected_snapshot=expected,
                           response_request=request, **transport)
        response = observation_response(snapshot, request)
    except (ValueError, KeyError, TypeError, OSError):
        response.update(outcome='failed', evidence='ci_observe_v1 evidence unavailable or precondition refused', observation=None)
    return response


def adapter_bytes(options):
    """Stage a self-contained, selectable observer. Every executed helper byte is pinned.

    Credentials remain file paths, read only at invocation. No imports from the checkout,
    environment configuration or fixture routes survive into the installed executable.
    """
    C.require(not set(options) & {'since', 'captured_at', 'nightly_receipt', 'release_pr'},
              'Staged authority cannot freeze occurrence selectors')
    root = ROOT
    pieces = ['#!/usr/bin/env python3\nimport sys, types\nsys.dont_write_bytecode=True\n']
    def embed(name, source, prelude=''):
        pieces.append(name + '=types.ModuleType(' + repr(name) + ');' + name + '.__file__=__file__\n')
        if prelude:
            pieces.append(prelude + '\n')
        pieces.append('exec(' + repr(source) + ',' + name + '.__dict__)\n')
    embed('_VALUES', (root / 'scripts/performance/perflab/values.py').read_text())
    pieces.append("sys.modules['perflab']=types.ModuleType('perflab');sys.modules['perflab'].__path__=[]\n"
                  "sys.modules['perflab.values']=_VALUES\n")
    embed('_NIGHTLY', (root / 'scripts/nightly/nightly_github.py').read_text())
    embed('_SCHEMA', (root / 'scripts/backlog-check-triage.py').read_text())
    embed('_CI', (root / 'scripts/ci-health.py').read_text())
    pieces.append('_CI._CI_SCHEMA=_SCHEMA\n')
    schema = C.loads((root / 'schemas/performance-nightly-publish-v1.json').read_bytes())
    embed('_PRODUCTION', (root / 'scripts/ci-health-github.py').read_text(),
        '_PRODUCTION._CI=_CI;_PRODUCTION._NIGHTLY=_NIGHTLY;_PRODUCTION._CI_SCHEMA=_SCHEMA;'
        '_PRODUCTION._RECEIPT_SCHEMA=' + repr(schema) + ';_PRODUCTION._OCCURRENCE_SCHEMA='
        + repr(C.loads((root / 'schemas/ci-observe-occurrence-v1.json').read_bytes())))
    pieces.append('try:\n raw=sys.stdin.buffer.read(65537)\n _CI.require(len(raw)<=65536,"Adapter request bound exceeded")\n'
        ' response=_PRODUCTION.adapter_response(_CI.loads(raw),' + repr(dict(sorted(options.items()))) + ')\n'
        ' print(_CI.canonical(response).decode())\n'
        'except (ValueError,KeyError,TypeError,OSError):\n print("ci_observe_v1 request refused",file=sys.stderr)\n raise SystemExit(1)\n')
    result = ''.join(pieces).encode()
    C.require(len(result) <= RESPONSE_BYTES, 'Installed adapter byte bound exceeded')
    return result
