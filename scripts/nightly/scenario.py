#!/usr/bin/python3 -IB
"""Bounded durable admission adapter; heavy phases run only in the external worker."""
import argparse
import contextlib
import fcntl
import os
from pathlib import Path
import stat
import sys
import tempfile
import time

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path[:0] = [str(HERE), str(ROOT / 'scripts/performance')]
import scenario_contracts as wire  # noqa: E402
from perflab import executor  # noqa: E402
from perflab.values import ContractError, canonical_digest, document_bytes, loads_strict, require, sha256_bytes  # noqa: E402

LIMIT = 65536
OPS, CONDITIONS = wire.OPERATIONS, wire.CONDITIONS
REVERSE = {v: k for k, v in CONDITIONS.items()}


def now_ms():
    return int(time.time() * 1000)


def private(path, directory=False):
    path = Path(path)
    require(path.is_absolute() and path.resolve() == path, 'machine path contains a link or alias')
    info = path.lstat()
    require(info.st_uid == os.getuid() and stat.S_IMODE(info.st_mode) == (0o700 if directory else 0o600)
            and (stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode)),
            'machine file must be private and owned by the worker account')
    for parent in path.parents:
        info = parent.lstat()
        require(stat.S_ISDIR(info.st_mode) and info.st_uid in (0, os.getuid())
                and (not info.st_mode & 0o022 or info.st_mode & stat.S_ISVTX), 'unsafe machine ancestor')
    return path


def read(path, limit=LIMIT):
    private(path)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        require(info.st_uid == os.getuid() and stat.S_IMODE(info.st_mode) == 0o600
                and stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and info.st_size <= limit,
                'not a bounded private regular file')
        data = stream.read(limit + 1)
        require(len(data) <= limit, 'machine document exceeds bound')
        return data


def load(path, name):
    return wire.validate(loads_strict(read(path), name, LIMIT), name)


def atomic(path, value, name):
    data = document_bytes(wire.validate(value, name))
    require(len(data) <= LIMIT, 'nightly document exceeds bound')
    path = Path(path)
    with tempfile.NamedTemporaryFile(dir=path.parent, prefix='.write-', delete=False) as out:
        temporary = Path(out.name)
        try:
            out.write(data)
            out.flush()
            os.fsync(out.fileno())
            os.replace(temporary, path)
            executor.sync_dir(str(path.parent))
        finally:
            if temporary.exists():
                temporary.unlink()


@contextlib.contextmanager
def lock(path):
    private(path)
    with open(path, 'rb') as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise executor.JournalBusy('nightly service busy') from error
        yield


def trusted_key(path):
    wanted = (ROOT / 'crates/af/keys/nightly.pub').read_text().splitlines()[1]
    found = Path(path).read_text().splitlines()
    require(len(found) == 2 and found[1] == wanted, 'nightly public key differs from embedded trust')


class Service:
    def __init__(self, path):
        self.config = load(path, 'nightly-service-v1.json')
        self.digest = canonical_digest(self.config)
        self.root = private(self.config['root'], directory=True)
        private(self.root / 'jobs', directory=True)
        for key in ('nightly_config', 'executor_config'):
            require(sha256_bytes(read(self.config[key])) == self.config[key + '_digest'], 'installed configuration changed')

    def path(self, tag):
        wire.TAG.validate(tag, 'nightly_tag')
        wire.night_date(tag[8:].replace('.', '-'))
        return self.root / 'jobs' / (tag + '.json')

    def job(self, tag):
        job = load(self.path(tag), 'nightly-job-v1.json')
        require(job['nightly_tag'] == tag and job['configuration_digest'] == self.digest, 'foreign nightly job')
        return job

    def save(self, job):
        with lock(self.root / 'queue.lock'):
            held = self.job(job['nightly_tag'])
            require(set(held['phases']) == set(job['phases']), 'job phases changed during worker execution')
            atomic(self.path(job['nightly_tag']), job, 'nightly-job-v1.json')

    def completion_path(self, tag, op):
        self.path(tag)
        require(op in OPS, 'invalid nightly phase')
        return self.root / 'jobs' / (tag + '-' + op + '.json')

    def observation(self, job, op):
        phase = job['phases'].get(op)
        state = 'pending' if phase is None else phase['state']
        evidence = 'awaiting worker' if phase is None else phase['detail']
        if state == 'done':
            completion = load(self.completion_path(job['nightly_tag'], op), 'nightly-completion-v1.json')
            require(canonical_digest(completion) == phase['evidence'] and completion['nightly_tag'] == job['nightly_tag']
                    and completion['operation'] == op and completion['configuration_digest'] == self.digest,
                    'nightly completion certificate changed')
            evidence = document_bytes(completion).decode()
        return state, dict(subject=job['nightly_tag'], revision=self.digest, condition=CONDITIONS[op],
                           satisfied=state == 'done', evidence=evidence)

    def request(self, request):
        require(isinstance(request, dict) and set(request) == {'schema', 'method', 'intent_id', 'input_digest',
                'operation', 'destination', 'inputs'} and request['schema'] == 'af/scenario-adapter-request@7',
                'unsupported adapter request')
        for key in ('intent_id', 'input_digest'):
            wire.DIGEST.validate(request[key], key)
        require(request['destination'] == 'github:' + self.config['repository'], 'unscoped release destination')
        inputs = request['inputs']

        def typed(key, kind='text'):
            value = inputs.get(key)
            require(isinstance(value, dict) and set(value) == {'type', 'value'} and value['type'] == kind,
                    'missing typed nightly input ' + key)
            return value['value']

        tag = typed('subject')
        self.path(tag)
        require(typed('revision') == self.digest, 'nightly service differs from pin')
        response = dict(schema='af/scenario-adapter-response@3', intent_id=request['intent_id'],
            input_digest=request['input_digest'], outcome='pending', evidence='durable nightly job', observation=None)
        op, method = request['operation'], request['method']
        with lock(self.root / 'queue.lock'):
            if method == 'observe':
                require(op == 'observe' and set(inputs) == {'subject', 'revision', 'condition'}, 'unauthorized observation')
                condition = typed('condition')
                require(condition in REVERSE, 'unknown condition')
                if not self.path(tag).exists():
                    return response
                state, observation = self.observation(self.job(tag), REVERSE[condition])
                response.update(outcome='done' if state == 'done' else 'failed' if state in ('failed', 'uncertain')
                                else 'pending', evidence=observation['evidence'], observation=observation)
                return response
            require(method in ('reconcile', 'execute') and op in OPS, 'unauthorized nightly operation')
            index = OPS.index(op)
            require(set(inputs) == {'subject', 'revision', 'nightly_tag'} | ({'evidence'} if index else set())
                    and typed('nightly_tag', 'nightly_tag') == tag, 'nightly Action inputs differ')
            if self.path(tag).exists():
                job = self.job(tag)
            else:
                require(index == 0, 'predecessor job absent')
                created = now_ms()
                job = dict(schema='af/nightly-job@1', nightly_tag=tag, configuration_digest=self.digest,
                    created_unix_ms=created, deadline_unix_ms=created + self.config['max_job_wall_ms'], phases={})
            if index:
                previous = typed('evidence', 'observation')
                state, expected = self.observation(job, OPS[index-1])
                require(state == 'done' and previous == expected, 'predecessor evidence changed or incomplete')
            if op in job['phases']:
                phase = job['phases'][op]
                require(phase['inputs_digest'] == canonical_digest(inputs), 'phase inputs changed under same tag')
                response.update(outcome='failed' if phase['state'] in ('failed', 'uncertain') else 'done', evidence=phase['detail'])
                return response
            if method == 'reconcile':
                response.update(outcome='absent', evidence='positive absence of exact durable phase')
                return response
            require(now_ms() <= job['deadline_unix_ms'], 'nightly deadline exhausted')
            require(len(list((self.root / 'jobs').glob('nightly-????.??.??.json'))) < self.config['max_jobs']
                    or self.path(tag).exists(), 'job capacity exhausted; retain evidence')
            job['phases'][op] = dict(intent_id=request['intent_id'], input_digest=request['input_digest'],
                inputs_digest=canonical_digest(inputs), state='queued', started_unix_ms=None,
                detail='durably queued; no computation or publication claim', evidence=None)
            atomic(self.path(tag), job, 'nightly-job-v1.json')
            response.update(outcome='done', evidence='durably queued ' + op + ' for ' + tag)
            return response

    def finish(self, job, op, artifacts, detail):
        completion = dict(schema='af/nightly-completion@1', nightly_tag=job['nightly_tag'],
            configuration_digest=self.digest, operation=op, artifacts=artifacts, detail=detail)
        atomic(self.completion_path(job['nightly_tag'], op), completion, 'nightly-completion-v1.json')
        job['phases'][op].update(state='done', evidence=canonical_digest(completion), detail=detail)
        self.save(job)

    def worker(self, backend=None):
        # Bounded cycle; systemd invokes again. Never spawn a detached child from an adapter.
        with lock(self.root / 'worker.lock'):
            backend = backend or Native(self)
            for path in sorted((self.root / 'jobs').glob('nightly-????.??.??.json')):
                job = self.job(path.stem)
                for op in OPS:
                    phase = job['phases'].get(op)
                    if phase is None or phase['state'] in ('done', 'failed', 'uncertain'):
                        continue
                    resumed = phase['state'] == 'running'
                    if now_ms() > job['deadline_unix_ms']:
                        phase.update(state='failed', detail='nightly wall budget exhausted; evidence retained')
                        self.save(job)
                        return 'failed'
                    phase.update(state='running', started_unix_ms=phase['started_unix_ms'] or now_ms(), detail='external worker owns phase')
                    self.save(job)
                    try:
                        require(executor.tree_usage(str(self.root)) <= self.config['max_bytes'], 'nightly byte budget exhausted')
                        artifacts, detail = backend.run(job, op, resumed)
                        require(now_ms() <= job['deadline_unix_ms'], 'nightly deadline exhausted')
                        require(executor.tree_usage(str(self.root)) <= self.config['max_bytes'], 'nightly byte budget exhausted')
                        if artifacts is None:
                            phase['detail'] = detail
                            self.save(job)
                            return 'pending'
                        self.finish(job, op, artifacts, detail)
                        return 'done'
                    except executor.JournalBusy:
                        phase['detail'] = 'native executor busy; phase retained'
                        self.save(job)
                        return 'busy'
                    except (ContractError, OSError, RuntimeError) as error:
                        phase.update(state='failed', detail=('nightly phase refused: ' + str(error))[:4096])
                        self.save(job)
                        return 'failed'
        return 'idle'


# Imported only by the separately supervised worker, never the bounded adapter.
def Native(service):
    from native_release import Native as Backend
    return Backend(service)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('adapter', 'worker'))
    parser.add_argument('--service', required=True)
    args = parser.parse_args()
    os.umask(0o077)
    try:
        service = Service(args.service)
        if args.command == 'worker':
            print(service.worker())
        else:
            request = loads_strict(sys.stdin.buffer.read(LIMIT + 1), 'nightly request', LIMIT)
            try:
                response = service.request(request)
            except executor.JournalBusy:
                response = dict(schema='af/scenario-adapter-response@3', intent_id=request['intent_id'],
                    input_digest=request['input_digest'], outcome='pending', evidence='nightly queue busy', observation=None)
            sys.stdout.buffer.write(document_bytes(response))
    except (OSError, ContractError, RuntimeError) as error:
        print('nightly service refused: ' + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
