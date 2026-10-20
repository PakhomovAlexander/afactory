# Telemetry

Telemetry records what `af` does while it works, in a store of its own under the state root,
never in a Task or Campaign Store. It is for the operator: where the wall time of a run went,
what failed and why, what a Worker saw and did. The boundaries are
[ADR-0153](adr/0153-record-telemetry-in-its-own-store-and-never-let-it-steer.md); the full
direction, with the packages still to come, is the design note
[`design/telemetry.md`](design/telemetry.md).

The kernel never reads it. Identity, replay, the Ledger, budgets, admission and every Worker
input are the same whether Telemetry is on, off or deleted, and nothing it captures reaches a
prompt. Nothing leaves the machine: this release has no sink, and the `sink` keys are refused
at load as not implemented.

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
labelled *perturbing* by `af telemetry status`: it keeps blobs of what Workers saw and did.

### The segment header

Each segment begins with one header record (`af/TelemetrySegment@1`). Its fields by level:

| Field | `metrics` | `spans` and `content` |
| --- | --- | --- |
| `segment_id`, `af_version`, `af_commit`, `pid`, `parent_pid`, `host`, `started_unix_ns`, `mono_origin_unix_ns`, `level` | recorded | recorded |
| `argv` | `argv[0]` and the subcommand path only (`af task show`): no option, no other argument | the whole command line; the value after an option named like a secret (`--api-key`, `--password`, `--pw`) or after `-p` is `[withheld]`, and every argument is judged whole by the credential rules below |
| `cwd` | `""`: not recorded | the working directory, judged whole, home prefix as `~` |

## Credentials

Credential-looking material is withheld at every level, before any bound can cut it, by a
bounded heuristic: the same rules `af` applies to a Provider's own words
([`providers.md`](providers.md) lists them). It recognises the common shapes of keys, tokens,
passwords and login challenges; it is not a promise to recognise every secret.

- A text line that looks like a credential (an assignment to a `key`, `token`, `secret`,
  `password`, `passwd`, `passphrase`, `credential` or `auth` name, a password after whitespace,
  a URL carrying a password, a bearer token, a token prefix such as `sk-` or `ghp_`, an AWS
  access key id, a JSON Web Token, a long hex, base64 or lower-case run, a device code, the
  account a login holds, login-challenge material, two long words that are one secret split by a
  space) becomes `[line withheld: looks like credential material]`. A `sha256:` digest and a
  40- or 64-hex git object id are no credentials and pass.
- In a JSON document a member named like a secret keeps its key and loses its value
  (`{"password":"[withheld]"}`, `{"pw":"[withheld]"}`); every other string is judged whole by the
  line rules, whatever its member is named (`{"url":"[withheld]"}` for
  `https://alice:hunter2@…`); an object holding a member whose name itself looks like a
  credential becomes the string `"[withheld]"`. The document still parses.
- A line or JSON document too long to be judged whole is withheld whole, never cut.
- A record attribute whose name itself looks like a credential (a token prefix, a JSON Web
  Token, a long secret run) is never written: the attribute is omitted and counted in
  `af.redacted`, and the record's other attributes stay.
- A record attribute named like a secret (`api_key`, `session_token`, `db_password`) keeps its
  name and loses its value, whatever its shape (text, number, identity, list), counted in
  `af.redacted`; only the counters and enumerations `af`'s own schema names this way (such as
  `af.usage.input_tokens`) keep a value of their type. On a command line, the argument after an
  option named like a secret (`--api-key VALUE`) is withheld the same way.
- Every other field of every record (span and event names, node, Finding and Demand fields,
  configuration rows) is either a kernel identity in an identity position or judged whole by
  these rules.
- An identity is exempt from these rules only when two things hold: the attribute or field is
  one of the identity positions the record declares (`af.store_id`, `af.run_id`, `af.task_id`,
  `af.attempt_id`, `af.execution_id`, `af.node_id`, `af.corr`, `trace_id`, `span_id`,
  `parent_span_id`, the digest fields such as `af.context.manifest` and `plan_digest`, and the
  like), and its value is exactly one of the kernel's identity grammars: a `sha256:` digest of
  64 lowercase hex digits (or a Task's run id, `task:` and a digest), a lowercase-hex id of 16,
  32 or 64 digits (span ids and `af.corr`, trace ids, Store ids), a segment id, an occurrence id
  `segment_id:seq`, or a decimal ordinal. Every other value, an identity-shaped one included
  (`af.run_id = "sk-proj-…"`), is judged as untrusted text, so a credential is withheld there as
  anywhere; a 32-hex id outside an identity position is a long hex run and is withheld too,
  while a digest there passes the text rules like any other digest.
- Token and charge counters are exact decimal strings only. They are, by name and then by
  family (`*` is any text): `af.usage.input_tokens`, `af.usage.output_tokens`,
  `af.usage.cache_read_tokens`, `af.usage.cache_write_tokens`, `af.usage.reasoning_tokens`,
  `af.usage.chargeable_tokens`, `af.admission.reserved_tokens`, `af.context.estimated_tokens`,
  `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `gen_ai.client.token.usage`,
  `af.store.sequence_at_start`, `af.usage.*_tokens`, `gen_ai.usage.*`, `af.charge*`. Any
  other value under such a
  name (text, even `"42"`, an identity, an enumeration, a boolean, an integer, a negative, a
  float, a list or a non-canonical decimal) is dropped and counted in `af.dropped_fields`
  before any other rule runs, so it is never withheld under the secret-name rule and never
  written as a JSON number.
  Timestamps are decimal strings too.
- A configuration row whose key is named like a secret (`provider.password`, `sink.auth_token`)
  or passes for a credential itself (`sk-proj-…`, a JSON Web Token, a long secret run, an
  assignment such as `API_KEY=…`) withholds the whole pair: the value and the origin become
  `[withheld]` whatever their length or shape, a credential-shaped key becomes `[withheld]` as
  well, and the pair counts once; other rows keep their values under the rules above. Each
  `config` record counts what it withheld in `af.redacted`.
- Environment values are never recorded; the home directory prefix becomes `~`. When `HOME`
  gives no usable prefix (unset, not absolute, `/`), the segment records one `redaction`
  problem saying home paths are not replaced, never a silent omission.
- A Provider's granted path becomes `<grant:NAME>` in every spelling: as given, through the
  home prefix (`~/.claude-work`), resolved through links, JSON-escaped (`\/Users\/…`),
  percent-encoded (`%2FUsers%2F…`), and written through `.`, `..` or doubled separators
  (`/Users/alice/./.claude-work`).

Each blob occurrence records the rules its bytes passed, `af-redact/2`.

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
| `max_bytes` | `"4GiB"` | `256MiB` to `[storage] max_bytes`; advisory on this release: `af telemetry status` warns when the directory is over it, and nothing removes telemetry to meet it |
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
| `sink` | `"none"` | `none`; `otlp` is refused, not implemented in this release |
| `sink_endpoint` | `""` | empty; a URL is refused, not implemented in this release |
| `sink_headers_file` | `""` | empty; a path is refused, not implemented in this release |
| `sink_timeout_ms` | `5000` | 100 to 60000; reserved, no sink reads it |

There is no telemetry `keep_days`: collection by age is `[storage] keep_days` under
`[storage] auto_gc` or an applied prune, as for every kind of entry
([ADR-0144](adr/0144-hold-afs-disk-use-to-a-machine-budget.md)).

## `af telemetry status`

```text
af telemetry status [--json] [--dir PATH]
```

Prints the configured level; the telemetry directory and its allocated bytes, in total against
`[telemetry] max_bytes` and per entry (each segment with its manifest; the blobs, which are only
the two-hex prefix directories under `v1/blobs/` and the digest-named objects and publishers'
temporary files in them; the index with its WAL and shared-memory files; temporary files; locks;
and `unrecognized`: every other file or directory beneath the telemetry directory, `v1/blobs/`
included — a stray name, and a symbolic link wherever it lies, which is never followed —
counted once with its count, its allocated bytes and up to 16 of its names, never read
and never removed, so the total is every allocated byte beneath the directory), each sealed
segment with its seal's `exit` and whether its capture is `complete` or why not (`recovered, drops unknown` for a
segment recovered without its writer's seal, `recovered`, `12 dropped`); the segments by state —
`live` (a writer holds its lock), `unsealed` (nobody holds it: a crashed writer's, recoverable),
`sealed` and `corrupt` (interior corruption, never read); the runs the index knows; the Attempts
whose content was withheld; the stored occurrences whose blob file is missing (reported as
`content unavailable: missing blob`, never a failed query), looked for with `gc.lock` held
shared — when a sweep keeps it longer than two seconds the check is reported `deferred` and no
blob file is looked at; the index after this command
ingested every new segment byte; the distinct problems recorded; what cannot be read (a
directory of another telemetry version such as `v0/` is listed and never read); and every
telemetry directory registered on this machine, with its overlap rechecked: `none`,
`overlapping` with the reason, or `unknown` when the Store registry (`stores.toml`) cannot be
read or is of another version — then every directory's check is `unknown`, the registry problem
is named among the problems (`registry`), and the exit code is unchanged. It never recovers or
removes a segment.

When the directory holds more than `[telemetry] max_bytes`, `status` says so on stderr and marks
the bytes line `over`; the cap is advisory on this release. `--json` prints one
`af/telemetry-status@1` document
([`schemas/telemetry-status-v1.json`](../schemas/telemetry-status-v1.json)); its paths are
written as the text output writes them, a path under the home directory as `~/…`.
`--dir PATH` reads
another telemetry directory, such as one copied from another machine. Exit codes: 0; 1 when the
directory cannot be read (or `--dir` names none) or its index is of another schema version;
2 for a wrong command line or a refused `[telemetry]` key.

A `.jsonl` segment in which an invalid line precedes a valid one is unreadable at once: none of
it is indexed, and it is listed as unreadable until `reindex` renames it `.corrupt`. A line over
the 16 KiB record bound is an invalid line, so a sealed segment holding one is refused and
listed, never indexed. An `index.sqlite` of another schema version (`af/telemetry-index@2`) is
refused, never migrated or read around: `status` exits 1 naming the file and both schemas, and
`af telemetry reindex` replaces it. Only when another command is replacing the index does
`status` read the segments directly, saying so on stderr; its answer is the same. The index
remembers each sealed segment file it read by its identity (device, inode, size and
modification time): a `.jsonl.gz` replaced or rewritten under its name since is read and
validated again, and one that no longer reads has its earlier rows dropped and is listed as
unreadable, so `status` answers as `reindex` would.

## `af telemetry reindex`

```text
af telemetry reindex [--json] [--dir PATH]
```

Recovers every unsealed segment whose lock nobody holds — keeping its longest prefix of
complete, valid lines, recording the discarded bytes in a seal with `exit = "recovered"`,
marking spans it never ended with no end — or renames it `.corrupt` when an invalid line
precedes a valid one. A recovered seal whose writer never sealed has `dropped` and
`dropped_chunks` `null`: what the writer dropped is unknown, and the capture is incomplete. A
writer that died after writing its seal but before compressing has that seal replaced by the
recovered one, which keeps the writer's counts, so the segment still ends with exactly one seal
and never reads as a clean exit. Then it replaces the index under `index.lock` (taken
exclusively within two seconds; new readers wait on `index.intent` meanwhile), rebuilds it from
every segment and verifies every blob file's digest with `gc.lock` held shared for the whole
scan; when that lock is not had within two seconds no blob file is opened and the document says
`blob_verification = "deferred"` instead of reporting corruption, with the same exit code.
Every query answers the same before and after: the index is derived state, never evidence.
`--json` prints one `af/telemetry-reindex@1` document
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
  carrying `seq`, `t` and `schema`. An attribute value over its bound (1 KiB for a string, 32
  elements or 4 KiB for an array of strings) becomes, at `content`, a blob occurrence the
  attribute names (an array as one JSON document); at every other level it is cut at the bound
  with a trailing `…`, counted in `af.truncated_fields`. The lock, never the PID in the name,
  says whether a writer is alive.
- **Blobs.** A blob's identity is the digest of the redacted, bounded bytes it keeps; each
  capture of it is its own occurrence record, so two sources that redact to the same bytes share
  one file and keep two occurrences. An occurrence carries its source's identity (the digest
  and size of the original bytes, before any redaction), its media type and the redaction its
  bytes passed; the shared object holds only what is intrinsic to the bytes. A JSON source
  longer than `content_max_blob_bytes` is withheld whole before it is parsed; longer text is
  redacted line by line and cut head and tail. An array of strings over its bound is measured
  before it is encoded: one whose JSON document is longer than `content_max_blob_bytes` is
  withheld whole, keeping its source identity, before anything as large as it is held. Blob
  collection acts only on two-hex prefix directories and digest-named or temporary files that
  are real directories and regular files, relative to the opened directory, and never follows
  a symbolic link out of the telemetry directory; readers never open a segment, blob, lock or
  index file through one.
- **Confinement.** Beneath a telemetry directory the only pathname operation is the first
  open of the telemetry directory itself, once per command or writer (it may be reached
  through a symbolic link you chose); every file and directory beneath it is opened, created,
  locked, renamed, measured and removed relative to a descriptor held from that open, each
  directory opened from its parent without following a symbolic link. A directory renamed or
  replaced, by a link or by another directory, after it was opened therefore changes nothing
  outside the telemetry directory that was opened, and nothing is read from the replacement.
  - *First opens* (one pathname open of the root, then descriptors): `Confined::root`,
    `Confined::open_root`, `Confined::create_root`, `Layout::open_versioned`,
    `Confined::versioned`, `layout::create_versioned`, `layout::exclusive_within` (a lock file
    named by path: the registry's, or a fixture's sweep), the writer's first write
    (`Writer::open_file`), `status::status`, `status::reindex`, `Index::open`,
    `Index::direct`, `index::replace`, `reader::list`, `reader::recover`,
    `reader::recover_all`, `collect::collect`, `collect::roots`, `blob::stage` and
    `blob::reader::{exists, read, content}`; `reader::read_live` and `reader::read_sealed` read
    one file a caller names (fixtures) and are never called by the crate itself.
  - *Functions that hold descriptors* (they take the opened directory, never a path): the
    writer's `Tree`, `Consumer`, `publish` and `seal_file`; `Index::open_in`,
    `Index::direct_in`, `index::replace_in`, `Index::ingest`, `Index::missing_blobs`;
    `reader::list_in`, `read_live_in`, `read_sealed_in`, `recover_in`, `recover_all_in`,
    `manifest_digests_in`; `collect::collect_in`, `collect::roots_in`; `blob::stage_in`,
    `blob::place`, `blob::reader::exists_in`, `blob::reader::read_in`; `layout::lock_in`; and
    `status`'s `verify_blobs`, `blob_bytes` and `unrecognized`. `status` and `reindex` open
    `v1/` once and hand it to the index, to recovery, to the blob checks and to direct reading
    when `index.lock` is not had in time.
  - *The index database.* SQLite opens files only by path, so it is given a path that names
    the held `v1/` itself, never its pathname: on macOS its inode path
    `/.vol/<device>/<inode>/index.sqlite` with `SQLITE_OPEN_NOFOLLOW`, and where the file
    system offers no inode path the open is refused with an error naming the index (fail
    closed); on Linux the descriptor's own entry `/proc/self/fd/<fd>/index.sqlite`, which
    SQLite expands itself inside its open (it treats that entry as a link, so
    `SQLITE_OPEN_NOFOLLOW` cannot be set there), and the connection is refused unless the
    file SQLite resolved is in the directory the descriptor names now; on any other system the
    open is refused. The database, WAL and shared-memory files are opened together and must be
    regular files of the held `v1/`.
  - *Outside the telemetry directory*, by path: the directory registry, its lock and the
    canonical paths it records (each registered directory's existence as `status` reports it), a Store's canonical path for
    `af.store_id`, and the free-space figure a segment header reads before the writer's first
    open.

  A blob whose prefix directory is a link reads as `content unavailable: missing blob` and
  `status` counts it missing.
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
