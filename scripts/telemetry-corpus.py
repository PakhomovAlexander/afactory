#!/usr/bin/env python3
"""The Telemetry read-latency corpus and its three measurements (design §12).

    telemetry-corpus.py generate --out DIR [--seed N] [--runs N] [--jobs N]
    telemetry-corpus.py measure --dir DIR [--af PATH] [--repeat N] [--json] [--report PATH]

`generate` writes `telemetry-corpus-10k` deterministically from a seed into DIR, a telemetry
directory the operator names (absent or empty; never af's state root): 10,000 runs over 20,000
executions, one sealed segment each, 2,000,000 spans, 4,000,000 events and 200,000 blobs of which
a quarter are shared by two executions of different runs, 2 GiB of segments and 1 GiB of blobs,
written at `level = "content"`. Segment bytes are the JSONL a writer wrote, what
`segment_max_bytes` bounds; blob bytes are the retained bytes, what the content bounds count;
both are compressed on disk as af compresses them. `--runs N` scales every count and byte total
by N/10,000; the same seed and N give byte-identical files with one zlib build. Every record is one this release
writes (`af/Telemetry*@1`, each span id derived from its segment and `seq`), so af reads the
corpus as it reads its own.

`measure` runs, against DIR with a private `HOME` and XDG directories: `af telemetry status`
on a fresh index (every segment ingested), `af telemetry status` with nothing new, and
`af telemetry show` of one run, each with its wall time from exec to exit, its peak memory and
the index's size after it. Each is measured warm (every file read once before) and, where the
platform lets this process drop the page cache (Linux as root, macOS as root through `purge`),
cold; otherwise the report says warm-only and why. The bounds of design §12 — 2 s fresh, 200 ms
unchanged, 200 ms `show` — are checked on the warm figures; the cold ones are reported beside
them. Prints a Markdown table, or with `--json` the `af/telemetry-read-latency@2` document
(`schemas/telemetry-read-latency-v2.json`). Exit status: 0 within every bound, 3 when a warm
figure is over its bound, 1 when the corpus cannot be generated or measured.
"""
import argparse
from contextlib import contextmanager, nullcontext
import fcntl
import gzip
import hashlib
import json
import multiprocessing
import os
from pathlib import Path
import platform
import random
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time

SCHEMA = 'af/telemetry-read-latency@2'
REPOSITORY = Path(__file__).resolve().parent.parent
FULL_RUNS = 10000
DEFAULT_SEED = 20261021
# Per run, at every scale: two executions, 100 spans and 200 events per execution, 20 blobs of
# which 5 are shared, 2 GiB of segments and 1 GiB of blobs over the full 10,000 runs.
EXECUTIONS_PER_RUN = 2
SPANS_PER_EXECUTION = 100
EVENTS_PER_EXECUTION = 200
UNSHARED_BLOBS_PER_RUN = 15
SHARED_BLOBS_PER_RUN = 5
SEGMENT_BYTES = 2 << 30
BLOB_BYTES = 1 << 30
NODES = 19
BOUNDS_MS = {'fresh_status': 2000, 'unchanged_status': 200, 'show': 200}
REDACTION = 'af-redact/1'
TEXT = 'text/plain; charset=utf-8'
START_NS = 1_790_000_000_000_000_000
WORDS = ('the kernel admits a typed result from every attempt and records its exact usage while '
         'the worker reads the captured source and reports what it found in each file under '
         'review with a bounded context manifest pipeline node check gate snapshot ledger '
         'finding demand evidence round epoch provider model harness stream turn tool call '
         'output contract reservation charge settle release lease store event sequence').split()


class CorpusError(Exception):
    """A corpus that cannot be generated or measured; the message names it."""


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


def number(seed, *parts):
    return int.from_bytes(hashlib.sha256(':'.join(map(str, (seed,) + parts)).encode())
                          .digest()[:8], 'big')


# ------------------------------------------------------------------------------------------
# the layout


class Layout:
    """Every count and identity of a corpus of `runs` runs from `seed`."""

    def __init__(self, seed, runs):
        self.seed = seed
        self.runs = runs
        self.executions = runs * EXECUTIONS_PER_RUN
        self.unshared = runs * UNSHARED_BLOBS_PER_RUN
        self.shared = runs * SHARED_BLOBS_PER_RUN
        self.blobs = self.unshared + self.shared
        self.segment_target = SEGMENT_BYTES * runs // FULL_RUNS // self.executions
        self.blob_mean = BLOB_BYTES * runs // FULL_RUNS // self.blobs

    def blob_size(self, index):
        # Uniform over [1 KiB, 2 * mean - 1 KiB]: the mean is the target.
        low = 1024
        return low + number(self.seed, 'size', index) % (2 * (self.blob_mean - low) + 1)

    def blob_segments(self, index):
        """The executions whose segments reference blob `index`."""
        if index < self.unshared:
            return [index % self.executions]
        shared = index - self.unshared
        first = (2 * shared) % self.executions
        return [first, (first + 1 + self.executions // 2) % self.executions]

    def store_id(self, run):
        return sha(f'corpus-store:{self.seed}:{run % 16}')

    def run_id(self, run):
        return f'task:sha256:{sha(f"corpus-run:{self.seed}:{run}")}'

    def start_ns(self, execution):
        run, ordinal = divmod(execution, EXECUTIONS_PER_RUN)
        return START_NS + run * 360_000_000_000 + ordinal * 120_000_000_000

    def segment_id(self, execution):
        started_ms = self.start_ns(execution) // 1_000_000
        pid = 20000 + execution % 40000
        return f'{started_ms}-{pid}-{sha(f"corpus-segment:{self.seed}:{execution}")[:8]}'


def expected_counts(seed, runs):
    """Index facts required by this generator, independent of any reader's output."""
    layout = Layout(seed, runs)
    return {'executions': layout.executions, 'segments': layout.executions,
            'blob_files': layout.blobs,
            'spans': layout.executions * SPANS_PER_EXECUTION,
            'events': layout.executions * EVENTS_PER_EXECUTION,
            'blobs': layout.blobs, 'shared_blobs': layout.shared,
            'blob_occurrences': sum(len(layout.blob_segments(i)) for i in range(layout.blobs)),
            'blob_bytes': sum(layout.blob_size(i) for i in range(layout.blobs)),
            'content_index': True, 'content_indexed': layout.blobs}


_POOL = {}


def pool(seed):
    """4 MiB of deterministic text the blobs are cut from."""
    if seed not in _POOL:
        rng = random.Random(f'corpus-pool:{seed}')
        lines = []
        size = 0
        while size < 4 << 20:
            line = ' '.join(rng.choice(WORDS) for _ in range(12)) + '\n'
            lines.append(line)
            size += len(line)
        _POOL[seed] = ''.join(lines).encode()
    return _POOL[seed]


def blob_bytes(layout, index):
    header = f'corpus blob {index}\n'.encode()
    size = layout.blob_size(index)
    text = pool(layout.seed)
    offset = number(layout.seed, 'offset', index) % (len(text) - size)
    return header + text[offset:offset + size - len(header)]


def write_blob(job):
    out, seed, runs, index = job
    layout = Layout(seed, runs)
    content = blob_bytes(layout, index)
    digest = hashlib.sha256(content).hexdigest()
    stored = gzip.compress(content, compresslevel=6, mtime=0)
    directory = Path(out) / 'v1' / 'blobs' / digest[:2]
    directory.mkdir(parents=True, exist_ok=True)
    (directory / f'{digest}.gz').write_bytes(stored)
    return index, f'sha256:{digest}', len(content), len(stored)


def dumps(record):
    return json.dumps(record, separators=(',', ':'), ensure_ascii=False)


def span_id(segment, seq):
    return hashlib.sha256(f'{segment}:{seq}'.encode()).hexdigest()[:16]


def segment_records(layout, execution, blobs, pad):
    """Every line of execution `execution`'s segment; `pad` extra artifact references are
    spread over its Store-event mirrors to reach the byte target."""
    run, ordinal = divmod(execution, EXECUTIONS_PER_RUN)
    segment = layout.segment_id(execution)
    store, run_id = layout.store_id(run), layout.run_id(run)
    trace = hashlib.sha256(f'af/trace@1:{store}:{run_id}'.encode()).hexdigest()[:32]
    clock = [layout.start_ns(execution)]
    lines = []

    def tick(step=1_000_000):
        clock[0] += step
        return str(clock[0])

    def add(record):
        record_seq = len(lines) + 1
        lines.append(dumps({'t': record.pop('t'), 'seq': record_seq, **record}))
        return record_seq

    def identity():
        return {'af.store_id': store, 'af.run_id': run_id, 'af.run.kind': 'task',
                'af.execution_id': segment}

    def start(name, parent, attributes, kind='internal'):
        seq = len(lines) + 1
        own = span_id(segment, seq)
        add({'t': 'span_start', 'schema': 'af/TelemetrySpan@1',
             'trace_id': None if parent is None else trace, 'span_id': own,
             'parent_span_id': parent, 'name': name, 'kind': kind, 'start_unix_ns': tick(),
             'attributes': attributes})
        return own

    def end(own, attributes, status='ok'):
        add({'t': 'span_end', 'schema': 'af/TelemetrySpan@1', 'span_id': own,
             'end_unix_ns': tick(), 'status': status, 'attributes': attributes})

    add({'t': 'segment', 'schema': 'af/TelemetrySegment@1', 'segment_id': segment,
         'af_version': 'corpus', 'af_commit': 'unknown', 'pid': 20000 + execution % 40000,
         'parent_pid': 1,
         'argv': ['af-telemetry-corpus', '--seed', str(layout.seed), '--runs',
                  str(layout.runs)],
         'cwd': '~', 'host': {'os': 'linux', 'arch': 'x86_64', 'cpus': 8,
                              'memory_bytes': None, 'load1': None, 'free_bytes': None},
         'started_unix_ns': str(clock[0]), 'mono_origin_unix_ns': str(clock[0]),
         'level': 'content'})
    command = start('af.command', None, {'process.command': 'af'})
    run_span = start('af.run', command, {
        **identity(), 'af.execution.ordinal': ordinal + 1,
        'af.store.sequence_at_start': str(ordinal * 400), 'af.task_id': f'corpus-{run:05d}',
        'af.epoch': ordinal + 1})
    authority = start('af.authority.load', run_span, {})
    end(authority, {'af.policy_rev': f'sha256:{sha(f"policy:{run}")}'})
    plan = start('af.plan', run_span, {'af.node_count': NODES})
    end(plan, {})
    sequence = ordinal * 400
    # Every event is a content event naming one of the segment's blobs or a Store-event mirror.
    store_events = EVENTS_PER_EXECUTION - len(blobs)
    per_node = [store_events // NODES + (1 if node < store_events % NODES else 0)
                for node in range(NODES)]
    pads = [pad // store_events + (1 if index < pad % store_events else 0)
            for index in range(store_events)]
    mirrored = 0
    written = set()
    for node in range(NODES):
        node_id = f'root.nodes.n{node:02d}'
        attempt_id = sha(f'attempt:{layout.seed}:{execution}:{node}')[:26]
        node_span = start('af.node', run_span, {'af.node_id': node_id, 'af.node.kind': 'verify'})
        attempt = start('af.attempt', node_span, {
            'af.attempt_id': attempt_id, 'af.node_id': node_id,
            'af.epoch': ordinal + 1, 'af.worker': f'corpus/worker-{node % 4}@1.0.0'})
        compose = start('af.context.compose', attempt, {})
        for blob_index in range(node, len(blobs), NODES):
            digest, size, stored = blobs[blob_index]
            if digest not in written:
                written.add(digest)
                add({'t': 'blob', 'schema': 'af/TelemetryBlob@1', 'digest': digest,
                     'bytes': size, 'stored_bytes': stored, 'redaction': REDACTION})
            occurrence_seq = len(lines) + 1
            add({'t': 'blob_ref', 'schema': 'af/TelemetryBlobRef@1',
                 'occurrence_id': f'{segment}:{occurrence_seq}', 'digest': digest,
                 'span_id': compose, 'event_seq': occurrence_seq + 1, 'source_digest': digest,
                 'original_bytes': size, 'truncated': False, 'state': 'stored',
                 'media_type': TEXT, 'redaction': REDACTION})
            add({'t': 'event', 'schema': 'af/TelemetryEvent@1', 'span_id': compose,
                 'time_unix_ns': tick(), 'name': 'gen_ai.content.input',
                 'attributes': {'af.context.bytes': size},
                 'blob': f'{segment}:{occurrence_seq}', 'blob_media_type': TEXT})
        end(compose, {'af.context.bytes': 4096 + node, 'af.context.estimated_tokens': 1024})
        harness = start('af.harness', attempt, {'gen_ai.operation.name': 'invoke_agent'},
                        kind='process')
        end(harness, {'process.exit_code': 0, 'af.stdout_bytes': 2048, 'af.stderr_bytes': 0})
        parse = start('af.output.parse', attempt, {})
        end(parse, {'af.outcome': 'valid'})
        for _ in range(per_node[node]):
            sequence += 1
            refs = [f'sha256:{sha(f"ref:{layout.seed}:{execution}:{sequence}:{k}")}'
                    for k in range(pads[mirrored])]
            attributes = {'af.store.event.type': 'TaskTransition@5',
                          'af.store.event.sequence': str(sequence)}
            if refs:
                attributes['artifact_refs'] = refs
            add({'t': 'event', 'schema': 'af/TelemetryEvent@1', 'span_id': attempt,
                 'time_unix_ns': tick(), 'name': 'af.store.event', 'attributes': attributes})
            mirrored += 1
        end(attempt, {'af.outcome': 'succeeded', 'af.usage.chargeable_tokens': str(1000 + node),
                      'af.usage.observation': 'complete'})
        end(node_span, {'af.outcome': 'succeeded'})
    sweep = start('af.sweep', command, {})
    end(sweep, {'af.sweep.evicted': 0})
    end(run_span, {'af.outcome': 'finished'})
    end(command, {**identity(), 'process.exit_code': 0})
    add({'t': 'seal', 'schema': 'af/TelemetrySeal@1', 'ended_unix_ns': tick(), 'exit': 'ok',
         'spans': SPANS_PER_EXECUTION, 'events': EVENTS_PER_EXECUTION, 'blobs': len(written),
         'dropped': 0, 'dropped_chunks': 0, 'discarded_bytes': 0, 'refs': f'{segment}.refs'})
    return ''.join(line + '\n' for line in lines).encode(), sorted(written)


def write_segment(job):
    out, seed, runs, execution, blobs = job
    layout = Layout(seed, runs)
    natural, _ = segment_records(layout, execution, blobs, 0)
    # One more artifact reference is 74 bytes (a digest, its quotes and a comma); the first of
    # an event adds its member's 17 too.
    pad = max(0, (layout.segment_target - len(natural)) // 80)
    data, referenced = segment_records(layout, execution, blobs, pad)
    segment = layout.segment_id(execution)
    directory = Path(out) / 'v1' / 'segments'
    stored = gzip.compress(data, compresslevel=6, mtime=0)
    (directory / f'{segment}.jsonl.gz').write_bytes(stored)
    (directory / f'{segment}.refs').write_text(''.join(f'{digest}\n' for digest in referenced))
    return execution, len(data), len(stored)


def under(path, root):
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def state_root():
    base = os.environ.get('XDG_STATE_HOME') or os.path.join(os.path.expanduser('~'),
                                                             '.local', 'state')
    return Path(base).resolve() / 'af'


def generate(args):
    out = Path(args.out).resolve()
    if under(out, state_root()) or under(state_root(), out):
        raise CorpusError(f'{out} overlaps af\'s state root {state_root()}; name another '
                          'directory')
    if out.exists() and any(out.iterdir()):
        raise CorpusError(f'{out} is not empty')
    if args.runs < 2:
        raise CorpusError('--runs must be at least 2')
    layout = Layout(args.seed, args.runs)
    (out / 'v1' / 'segments').mkdir(parents=True, exist_ok=True)
    (out / 'v1' / 'blobs').mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    jobs = max(1, args.jobs)
    # A real serial path needs neither a Pool nor SemLock (/dev/shm is absent under
    # --ipc none). Keep the same writers, ordering and bytes as parallel preparation.
    context = (nullcontext() if jobs == 1
               else multiprocessing.get_context('spawn').Pool(jobs))
    with context as workers:
        def dispatch(writer, items, chunksize):
            return (map(writer, items) if workers is None
                    else workers.imap_unordered(writer, items, chunksize))

        written = sorted(dispatch(
            write_blob, ((str(out), args.seed, args.runs, index)
                         for index in range(layout.blobs)), chunksize=256))
        table = [(digest, size, stored) for _, digest, size, stored in written]
        per_segment = [[] for _ in range(layout.executions)]
        for index in range(layout.blobs):
            for execution in layout.blob_segments(index):
                per_segment[execution].append(index)
        segments = sorted(dispatch(
            write_segment, ((str(out), args.seed, args.runs, execution,
                             [table[index] for index in per_segment[execution]])
                            for execution in range(layout.executions)), chunksize=16))
    summary = {
        'out': str(out), 'seed': args.seed, 'runs': layout.runs,
        'executions': layout.executions,
        'spans': layout.executions * SPANS_PER_EXECUTION,
        'events': layout.executions * EVENTS_PER_EXECUTION,
        'blobs': layout.blobs, 'shared_blobs': layout.shared,
        'segment_bytes': sum(size for _, size, _ in segments),
        'segment_stored_bytes': sum(stored for _, _, stored in segments),
        'blob_bytes': sum(size for _, size, _ in table),
        'blob_stored_bytes': sum(stored for _, _, stored in table),
        'seconds': round(time.monotonic() - started, 1),
    }
    print(json.dumps(summary, indent=2))
    return 0


# ------------------------------------------------------------------------------------------
# the measurements


def private_env(machine):
    env = {name: value for name, value in os.environ.items()
           if not name.startswith(('AF_', 'XDG_', 'OTEL_')) and name not in ('HOME', 'TMPDIR')}
    env.update({
        'HOME': str(machine / 'home'), 'XDG_CONFIG_HOME': str(machine / 'config'),
        'XDG_STATE_HOME': str(machine / 'state'), 'XDG_DATA_HOME': str(machine / 'data'),
        'XDG_CACHE_HOME': str(machine / 'cache'), 'XDG_BIN_HOME': str(machine / 'bin'),
        'TMPDIR': str(machine / 'tmp'), 'AF_SELF_OFFLINE': '1', 'NO_COLOR': '1',
        'AF_TELEMETRY__LEVEL': 'content', 'AF_TELEMETRY__CONTENT_INDEX': 'true',
    })
    return env


def index_files(corpus):
    return [corpus / 'v1' / name for name in ('index.sqlite', 'index.sqlite-wal',
                                              'index.sqlite-shm')]


def index_bytes(corpus):
    return sum(path.stat().st_size for path in index_files(corpus) if path.exists())


def drop_index(corpus):
    for path in index_files(corpus):
        if path.exists():
            path.unlink()


def cache_dropper():
    """How this process can drop the page cache, or why it cannot."""
    if sys.platform.startswith('linux'):
        if os.access('/proc/sys/vm/drop_caches', os.W_OK):
            def drop():
                os.sync()
                Path('/proc/sys/vm/drop_caches').write_text('3\n')
            return drop, 'linux /proc/sys/vm/drop_caches', None
        return None, None, 'dropping the page cache needs root on Linux'
    if sys.platform == 'darwin':
        purge = shutil.which('purge')
        if purge and os.geteuid() == 0:
            return (lambda: subprocess.run([purge], check=True)), 'macos purge', None
        return None, None, 'dropping the page cache needs root (`purge`) on macOS'
    return None, None, f'no page-cache drop on {sys.platform}'


def warm(corpus):
    """Read every file of the corpus once."""
    for path in sorted(corpus.rglob('*')):
        if path.is_file():
            with path.open('rb') as file:
                while file.read(1 << 20):
                    pass


def timed(argv, env, cwd):
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        start = time.perf_counter_ns()
        process = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                   stdout=stdout, stderr=stderr)
        _, status, usage = os.wait4(process.pid, 0)
        wall_ns = time.perf_counter_ns() - start
        process.returncode = os.waitstatus_to_exitcode(status)
        stdout.seek(0)
        stderr.seek(0)
        out, err = stdout.read(), stderr.read()
    if process.returncode != 0:
        raise CorpusError(f'{" ".join(argv[1:])}: exit {process.returncode}\n'
                          f'{err.decode(errors="replace")[-2000:]}')
    scale = 1 if sys.platform == 'darwin' else 1024
    return {'latency_ms': round(wall_ns / 1e6, 3), 'peak_rss_bytes': usage.ru_maxrss * scale,
            'exit_code': process.returncode}, out


def sample(argv, env, cwd, corpus, repeat):
    """The median of `repeat` runs of `argv`, with the index's size after them."""
    runs = [timed(argv, env, cwd)[0] for _ in range(repeat)]
    chosen = sorted(runs, key=lambda run: run['latency_ms'])[(len(runs) - 1) // 2]
    return {**chosen, 'index_bytes': index_bytes(corpus),
            'samples_ms': [run['latency_ms'] for run in runs]}


def trailer_size(path):
    """The uncompressed size a gzip member records in its trailer."""
    with path.open('rb') as file:
        file.seek(-4, os.SEEK_END)
        return int.from_bytes(file.read(4), 'little')


@contextmanager
def fact_index(corpus):
    """Query a private database/WAL copy without changing the measured index.

    Native macOS SQLite can refuse mode=ro on a WAL database after af's final connection
    checkpoints and removes its WAL/SHM (SQLITE_CANTOPEN, ENOENT on the first query).
    Let SQLite reconstruct sidecars only in private scratch. Copy a retained WAL too:
    immutable=1 would silently ignore its committed rows. Never copy shared-memory locks.
    Exclude af connections/replacement while copying, using its existing lifetime lock.
    """
    try:
        with (corpus / 'v1' / 'index.lock').open('rb') as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as error:
                raise CorpusError(f'{corpus}: index is in use while reading corpus facts') from error
            with tempfile.TemporaryDirectory(prefix='af-corpus-facts-') as scratch:
                snapshot = Path(scratch) / 'index.sqlite'
                shutil.copyfile(corpus / 'v1' / 'index.sqlite', snapshot)
                wal = corpus / 'v1' / 'index.sqlite-wal'
                if wal.exists():
                    shutil.copyfile(wal, Path(scratch) / wal.name)
                # The copy is complete and private; the original may be used again.
                fcntl.flock(lock, fcntl.LOCK_UN)
                connection = sqlite3.connect(snapshot.as_uri() + '?mode=rw', uri=True)
                try:
                    connection.execute('PRAGMA query_only=ON')
                    yield connection
                finally:
                    connection.close()
    except (sqlite3.Error, OSError) as error:
        files = []
        for path in index_files(corpus):
            try:
                stat = path.stat()
                files.append(f'{path.name}: {stat.st_size} bytes, mode {stat.st_mode & 0o777:o}')
            except OSError as problem:
                files.append(f'{path.name}: {problem}')
        code = getattr(error, 'sqlite_errorcode', 'unavailable')
        name = getattr(error, 'sqlite_errorname', 'unavailable')
        raise CorpusError(f'{corpus}: reading corpus facts: {error}; '
                          f'SQLite {sqlite3.sqlite_version}, code={code}, name={name}; '
                          + '; '.join(files)) from error


def corpus_facts(corpus):
    segments = sorted((corpus / 'v1' / 'segments').glob('*.jsonl.gz'))
    blobs = sorted((corpus / 'v1' / 'blobs').glob('*/*.gz'))
    if not segments:
        raise CorpusError(f'{corpus}: no sealed segment under v1/segments')
    with gzip.open(segments[0], 'rb') as file:
        header = json.loads(file.readline())
    argv = header.get('argv', [])
    seed = int(argv[argv.index('--seed') + 1]) if '--seed' in argv else None
    with fact_index(corpus) as connection:
        def count(query):
            return connection.execute(query).fetchone()[0]
        facts = {
            'runs': count('SELECT count(*) FROM runs'),
            'executions': count('SELECT count(*) FROM executions'),
            'spans': count('SELECT count(*) FROM spans'),
            'events': count('SELECT count(*) FROM events'),
            'blobs': count('SELECT count(*) FROM blobs'),
            'shared_blobs': count('SELECT count(*) FROM (SELECT digest FROM blob_refs '
                                  'GROUP BY digest HAVING count(DISTINCT segment_id) > 1)'),
            'blob_occurrences': count('SELECT count(*) FROM blob_refs'),
            'blob_bytes': count('SELECT coalesce(sum(bytes), 0) FROM blobs'),
        }
        run_id = connection.execute(
            'SELECT run_id FROM runs ORDER BY run_id LIMIT 1 OFFSET ?',
            (facts['runs'] // 2,)).fetchone()[0]
        # Whether the reads ran with the full-text index: the index has `content_fts` and
        # it indexed the corpus's text blobs, read from the index, never assumed.
        has_fts = count("SELECT count(*) FROM sqlite_master WHERE name = 'content_fts'") > 0
        indexed = (count("SELECT count(*) FROM content_index WHERE state = 'indexed'")
                   if has_fts else 0)
    name = ('telemetry-corpus-10k' if facts['runs'] == FULL_RUNS
            else f"telemetry-corpus-{facts['runs']}")
    return {'name': name, 'seed': seed, **facts,
            'segments': len(segments),
            'segment_bytes': sum(trailer_size(path) for path in segments),
            'segment_stored_bytes': sum(path.stat().st_size for path in segments),
            'blob_files': len(blobs),
            'blob_stored_bytes': sum(path.stat().st_size for path in blobs),
            'content_index': has_fts, 'content_indexed': indexed}, run_id


def measure(args):
    corpus = Path(args.dir).resolve()
    if not (corpus / 'v1' / 'segments').is_dir():
        raise CorpusError(f'{corpus}: not a telemetry directory (no v1/segments)')
    af = str(Path(args.af).resolve())
    machine = Path(tempfile.mkdtemp(prefix='af-telemetry-corpus-'))
    try:
        for name in ('home', 'config', 'state', 'data', 'cache', 'bin', 'tmp'):
            (machine / name).mkdir()
        env = private_env(machine)
        version = subprocess.run([af, '--version'], capture_output=True, text=True, env=env)
        if version.returncode != 0:
            raise CorpusError(f'{af} --version: {version.stderr.strip()}')
        status = [af, 'telemetry', 'status', '--dir', str(corpus), '--json']
        drop, method, reason = cache_dropper()
        cold = {}
        if drop is not None:
            drop_index(corpus)
            drop()
            cold['fresh_status'] = sample(status, env, machine, corpus, 1)
            drop()
            cold['unchanged_status'] = sample(status, env, machine, corpus, 1)
        warm(corpus)
        drop_index(corpus)
        fresh = sample(status, env, machine, corpus, 1)
        unchanged = sample(status, env, machine, corpus, args.repeat)
        facts, run_id = corpus_facts(corpus)
        show = [af, 'telemetry', 'show', run_id, '--dir', str(corpus), '--json']
        timed(show, env, machine)
        shown = sample(show, env, machine, corpus, args.repeat)
        if drop is not None:
            drop()
            cold['show'] = sample(show, env, machine, corpus, 1)
        _, printed = timed(status, env, machine)
        document = json.loads(printed)
        sealed = document.get('segments', {}).get('sealed')
        if sealed is not None and sealed != facts['segments']:
            raise CorpusError(f'af telemetry status counts {sealed} sealed segments of '
                              f"{facts['segments']}")
    finally:
        shutil.rmtree(machine, ignore_errors=True)
    load = os.getloadavg()[0] if hasattr(os, 'getloadavg') else None
    measurements = []
    for name, figure, argv in (('fresh_status', fresh, status),
                               ('unchanged_status', unchanged, status), ('show', shown, show)):
        measurements.append({
            'name': name,
            'command': ' '.join(['af'] + [str(corpus) if part == str(corpus) else part
                                          for part in argv[1:]]),
            'bound_ms': BOUNDS_MS[name], 'warm': figure, 'cold': cold.get(name),
            'within_bound': figure['latency_ms'] <= BOUNDS_MS[name]})
    return {
        'schema': SCHEMA,
        'af': {'path': af, 'version': version.stdout.strip()},
        'machine': {'os': platform.system().lower(), 'arch': platform.machine(),
                    'cpus': os.cpu_count() or 0,
                    'load1': None if load is None else round(load, 2)},
        'corpus': {'dir': str(corpus), **facts},
        'cold': {'available': drop is not None, 'method': method, 'reason': reason},
        'measurements': measurements,
        'passed': all(entry['within_bound'] for entry in measurements),
    }


def size(count):
    return f'{count / (1 << 30):.2f} GiB' if count >= 1 << 30 else f'{count / (1 << 20):.1f} MiB'


def table(document):
    corpus = document['corpus']
    lines = [f"Corpus `{corpus['name']}` (seed {corpus['seed']}): {corpus['runs']} runs, "
             f"{corpus['executions']} executions, {corpus['spans']} spans, {corpus['events']} "
             f"events, {corpus['blobs']} blobs ({corpus['shared_blobs']} shared), "
             f"{size(corpus['segment_bytes'])} of segments "
             f"({size(corpus['segment_stored_bytes'])} on disk), "
             f"{size(corpus['blob_bytes'])} of blobs ({size(corpus['blob_stored_bytes'])} on "
             'disk).', '',
             '| Measurement | Warm | Peak memory | Index | Cold | Bound (warm) | Verdict |',
             '| --- | ---: | ---: | ---: | ---: | ---: | --- |']
    for entry in document['measurements']:
        figure = entry['warm']
        cold = entry['cold']
        lines.append(
            f"| `{entry['name']}` | {figure['latency_ms']:.0f} ms | "
            f"{size(figure['peak_rss_bytes'])} | {size(figure['index_bytes'])} | "
            f"{'warm-only' if cold is None else format(cold['latency_ms'], '.0f') + ' ms'} | "
            f"{entry['bound_ms']} ms | {'within' if entry['within_bound'] else 'over'} |")
    if not document['cold']['available']:
        lines += ['', f"Cold figures not measured: {document['cold']['reason']}."]
    return '\n'.join(lines) + '\n'


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    commands = parser.add_subparsers(dest='command', required=True)
    make = commands.add_parser('generate', help='write the corpus into an empty directory')
    make.add_argument('--out', required=True)
    make.add_argument('--seed', type=int, default=DEFAULT_SEED)
    make.add_argument('--runs', type=int, default=FULL_RUNS)
    make.add_argument('--jobs', type=int, default=os.cpu_count() or 1,
                      help='preparation workers; 1 runs serially without multiprocessing IPC')
    read = commands.add_parser('measure', help='measure the three reads on a corpus')
    read.add_argument('--dir', required=True)
    read.add_argument('--af', default=str(REPOSITORY / 'target' / 'release' / 'af'))
    read.add_argument('--repeat', type=int, default=3,
                      help='warm runs of the unchanged status and of show; the median counts')
    read.add_argument('--json', action='store_true', help=f'print the {SCHEMA} document')
    read.add_argument('--report', help='also write the document to this file')
    args = parser.parse_args(argv)
    try:
        if args.command == 'generate':
            return generate(args)
        if args.repeat < 1:
            parser.error('--repeat must be at least 1')
        document = measure(args)
    except CorpusError as error:
        print(f'telemetry-corpus: {error}', file=sys.stderr)
        return 1
    if args.report:
        Path(args.report).write_text(json.dumps(document, indent=2) + '\n', encoding='utf-8')
    sys.stdout.write(json.dumps(document, indent=2) + '\n' if args.json else table(document))
    return 0 if document['passed'] else 3


if __name__ == '__main__':
    sys.exit(main())
