# Telemetry

Telemetry records what `af` does while it works, in a store of its own under the state root,
never in a Task or Campaign Store. It is for the operator: where the wall time of a run went,
what failed and why, what a Worker saw and did. The boundaries are
[ADR-0153](adr/0153-record-telemetry-in-its-own-store-and-never-let-it-steer.md); the full
direction, with the packages still to come, is the design note
[`design/telemetry.md`](design/telemetry.md).

The kernel never reads it. Identity, replay, the Ledger, budgets, admission and every Worker
input are the same whether Telemetry is on, off or deleted, and nothing it captures reaches a
prompt. Nothing leaves the machine unless an OTLP sink is configured.

## What this release records

This release ships the store, its configuration and the two commands below. **No command
records spans yet**: the kernel's own spans, the supervised processes, the harness streams, the
Store mirror and the content blobs arrive with the later packages of the design. Until then a
telemetry directory holds only what a fixture or a later release wrote, and `af telemetry status`
reports an empty or absent directory on a machine that ran only this release.

## Levels

`[telemetry] level` decides what is recorded:

| Level | Records |
| --- | --- |
| `off` | nothing: no directory, no subscriber, no registry entry |
| `metrics` | command, run, gate, check, node, Attempt, harness and sweep spans with identity and numeric attributes; failures; resource use |
| `spans` (default) | every span and event, the configuration record, the Store mirror and Finding summaries; sizes and digests of content, never content |
| `content` | `spans` plus content blobs: Worker inputs, assistant text, reasoning, tool arguments and results, check output |

`AF_TELEMETRY__LEVEL=content af task run TASK_ID` raises the level for one run. `content` is
labelled *perturbing* by `af telemetry status`: it keeps blobs and is not bounded by the
overhead gate.

## Credentials

Credential-looking material is withheld at every level, by the same rules `af` applies to a
Provider's own words, and before any bound can cut it:

- A text line that looks like a credential (an assignment to a `key`, `token`, `secret`,
  `password`, `passwd`, `credential` or `auth` name, a bearer token, a token prefix such as
  `sk-` or `ghp_`, a JSON Web Token, a long hex or base64 run, login-challenge material) becomes
  `[line withheld: looks like credential material]`.
- In a JSON document a member named like a secret keeps its key and loses its value
  (`{"password":"[withheld]"}`); an object holding a member whose name itself looks like a
  credential becomes the string `"[withheld]"`. The document still parses.
- A line or JSON document too long to be judged whole is withheld whole, never cut.
- Identities and counters `af` produced (digests, run, Attempt, segment and span ids, token
  counts as exact decimal strings, timestamps) are typed fields and stay intact.
- Environment values are never recorded; the home directory prefix becomes `~`.

Each blob records the rules it passed, `af-redact/1`.

## The `[telemetry]` table

Machine layers only: the built-in defaults, `/etc/af/config.toml`, the user's
`~/.config/af/config.toml` and `AF_TELEMETRY__<KEY>`. A directory, project or local
`.af/af.toml` that carries `[telemetry]` is reported (`af config show` prints
`# ignored: [telemetry] in …`) and ignored. A value outside its bound, or an unknown key, refuses
the load naming the key, and `af telemetry` then exits 2. `af help telemetry` prints the same
table.

| Key | Default | Bound |
| --- | --- | --- |
| `level` | `"spans"` | `off`, `metrics`, `spans`, `content` |
| `dir` | `""` (`$XDG_STATE_HOME/af/telemetry`) | an absolute path overlapping no Store, cache or other telemetry directory |
| `max_bytes` | `"4GiB"` | `256MiB` to `[storage] max_bytes` |
| `flush_bytes` | `"64KiB"` | `4KiB` to `16MiB` |
| `flush_latency_ms` | `1000` | 50 to 10000 |
| `observer_queue_bytes` | `"4MiB"` | `256KiB` to `64MiB` |
| `segment_max_bytes` | `"256MiB"` | `16MiB` to `4GiB`, the last 64 KiB reserved for the seal |
| `content_max_blob_bytes` | `"16MiB"` | `64KiB` to `256MiB`; larger content keeps head and tail |
| `content_attempt_max_bytes` | `"512MiB"` | `16MiB` to `8GiB` |
| `content_process_max_bytes` | `"64MiB"` | `4MiB` to `1GiB` |
| `content_native_body_max_chars` | `1048576` | 4096 to 8388608 |
| `content_reasoning` | `true` | boolean |
| `content_tool_io` | `true` | boolean |
| `content_model_bodies` | `true` | boolean |
| `content_index` | `true` | boolean |
| `harness_native` | `false` | boolean |
| `sink` | `"none"` | `none`, `otlp` |
| `sink_endpoint` | `""` | an `http://` or `https://` URL, required with `sink = "otlp"` and refused otherwise |
| `sink_headers_file` | `""` | a path |
| `sink_timeout_ms` | `5000` | 100 to 60000 |

There is no telemetry `keep_days`: collection by age is `[storage] keep_days` under
`[storage] auto_gc` or an applied prune, as for every kind of entry
([ADR-0144](adr/0144-hold-afs-disk-use-to-a-machine-budget.md)).

## `af telemetry status`

```text
af telemetry status [--json] [--dir PATH]
```

Prints the configured level; the telemetry directory and its allocated bytes, in total against
`[telemetry] max_bytes` and per entry (each segment with its manifest, the blobs, the index with
its WAL and shared-memory files, temporary files, locks); the segments by state — `live` (a
writer holds its lock), `unsealed` (nobody holds it: a crashed writer's, recoverable), `sealed`
and `corrupt` (interior corruption, never read); the runs the index knows; the Attempts whose
content was withheld; the stored occurrences whose blob file is missing (reported as
`content unavailable: missing blob`, never a failed query); the index after this command
ingested every new segment byte; the distinct problems recorded; what cannot be read (a
directory of another telemetry version such as `v0/` is listed and never read); and every
telemetry directory registered on this machine, with any overlap rechecked. It never recovers
or removes a segment.

`--json` prints one `af/telemetry-status@1` document
([`schemas/telemetry-status-v1.json`](../schemas/telemetry-status-v1.json)). `--dir PATH` reads
another telemetry directory, such as one copied from another machine. Exit codes: 0; 1 when the
directory cannot be read (or `--dir` names none); 2 for a wrong command line or a refused
`[telemetry]` key.

When another command is replacing the index, `status` reads the segments directly and says so
on stderr; its answer is the same.

## `af telemetry reindex`

```text
af telemetry reindex [--json] [--dir PATH]
```

Recovers every unsealed segment whose lock nobody holds — keeping its longest prefix of
complete, valid lines, recording the discarded bytes in a seal with `exit = "recovered"`,
marking spans it never ended with no end — or renames it `.corrupt` when an invalid line
precedes a valid one. Then it replaces the index under `index.lock` (taken exclusively within
two seconds; new readers wait on `index.intent` meanwhile), rebuilds it from every segment and
verifies every blob file's digest. Every query answers the same before and after: the index is
derived state, never evidence. `--json` prints one `af/telemetry-reindex@1` document
([`schemas/telemetry-reindex-v1.json`](../schemas/telemetry-reindex-v1.json)). Exit codes: 0;
1 when the directory cannot be read or another command holds the index; 2 as for `status`.

## The store

```text
$XDG_STATE_HOME/af/telemetry/
  v1/
    segments/<started_unix_ms>-<pid>-<nonce>.jsonl      live: one writer under an exclusive lock
    segments/<started_unix_ms>-<pid>-<nonce>.jsonl.gz   sealed: compressed at exit, immutable
    segments/<started_unix_ms>-<pid>-<nonce>.refs       sealed: the blob digests it references
    segments/<started_unix_ms>-<pid>-<nonce>.corrupt    interior corruption: never read
    blobs/<aa>/<sha256>.gz                               content-addressed, compressed, shared
    gc.lock                                              shared by publishers and readers, exclusive by a sweep
    index.lock, index.intent                             the index's lifecycle locks
    index.sqlite (+ -wal, -shm)                          rebuildable projection over the segments
$XDG_STATE_HOME/af/telemetry-dirs.v1.json                every telemetry directory ever written here
```

- **Segments.** One per process, opened at its first record and never before. Each line is one
  record (`af/TelemetrySegment@1`, `af/TelemetrySpan@1`, `af/TelemetryEvent@1`,
  `af/TelemetryConfig@1`, `af/TelemetryNode@1`, `af/TelemetryFinding@1`, `af/TelemetryDemand@1`,
  `af/TelemetryBlob@1`, `af/TelemetryBlobRef@1`, `af/TelemetryProblem@1`, `af/TelemetrySeal@1`;
  [`schemas/telemetry-record-v1.json`](../schemas/telemetry-record-v1.json)) of at most 16 KiB,
  carrying `seq`, `t` and `schema`. The lock, never the PID in the name, says whether a writer
  is alive.
- **Blobs.** A blob's identity is the digest of the redacted, bounded bytes it keeps; each
  capture of it is its own occurrence record, so two sources that redact to the same bytes share
  one file and keep two occurrences.
- **Registry.** Before its first write a process registers its telemetry directory
  (`af/TelemetryDirs@1`, [`schemas/telemetry-dirs-v1.json`](../schemas/telemetry-dirs-v1.json)).
  A directory inside or containing a Store, the warm check cache, a release installation or
  another telemetry directory is refused with one problem, and the run continues without
  telemetry.
- **Versions.** A release that changes a record shape writes `v2/`; this release lists any other
  version's directory as unreadable and never reads it.

## Not yet captured

Kernel phase spans, process resource use, the configuration record, the harness streams (turns,
tool calls, reasoning), the Store mirror and Finding summaries, content blobs, the harnesses' own
OpenTelemetry export, the `telemetry` kind in `af storage`, and the commands `runs`, `show`,
`tail`, `turns`, `tools`, `context`, `reasoning`, `config`, `ledger`, the analytics, `search`,
`sql`, `export` and `gc` all belong to later packages of the design note.
