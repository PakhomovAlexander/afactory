# ADR-0153: Record telemetry in its own store, and never let it steer

**Status:** accepted (2026-10-19).

## Context

The owner asked to see everything `af` does while it works: every kernel phase, every process it
supervises, every model turn, tool call and reasoning summary a harness exposes, the
configuration a run ran under, the events and Findings the Ledger is built from, the resources
each step used, and every failure with its cause. The design is
[`docs/design/telemetry.md`](../design/telemetry.md) (revision 6); its §3 proposes the
boundaries this ADR fixes. They come before any capture code, because a recorder this close to the
kernel can change what it records: it could become an input, a replay dependency, a budget, a
leak of credentials, or a disk that fills without bound.

Facts that bound the decision:

- The kernel's authority is the Store: identity, replay, the Ledger, convergence, budget
  reservations, admission and every Worker input
  ([ADR-0028](0028-prioritize-wise-token-use-and-minimum-worker-context.md),
  [ADR-0078](0078-bind-review-conclusions-to-exact-task-accounting.md)). A second source of truth
  beside it is a second thing to replay.
- `review-process` is the dependency-neutral leaf of process supervision
  ([ADR-0026](0026-share-process-supervision-through-a-leaf-crate.md)).
- Every byte `af` keeps between runs is held to one machine budget under one set of eviction
  rules ([ADR-0144](0144-hold-afs-disk-use-to-a-machine-budget.md)), and a release reads only what
  it writes ([ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)).
- The rules review-runner applies to Provider-authored text (`looks_like_credential`,
  `WITHHELD_LINE`) withhold every run of 32 hex characters, so they withhold every digest; a
  recorder that ran them over its own identities would destroy the join keys.
- A harness's successful reply can carry a device code beside a challenge URL, and the adapters
  take their whole-capture privacy decision only for a failed completion.

## Considered options

- **Telemetry tables in the Store's `events.sqlite`.** One file and one budget entry. Rejected:
  the Store is the kernel's authority; its collection, identity and replay must not see
  telemetry, and a large transcript beside a small event log threatens the Store's read times.
- **An in-process OpenTelemetry SDK with its exporters.** Rejected: it brings an async runtime
  and a TLS stack into a synchronous kernel for a wire format `af` writes with `serde_json`.
- **Configuration from any layer, like `[delivery]`.** Rejected: a repository's `.af/af.toml`
  would decide what a contributor's machine records and where it sends it.
- **Redacting after truncation, or reusing the Provider-text rules on every field.** Rejected:
  a bound can cut away the assignment name a credential is judged by, and the heuristic would
  withhold every digest and id.
- **Failing the run on a telemetry error.** Rejected: an unwritable directory would stop work
  that never depended on it. The free-disk floor of ADR-0144 still applies.
- **A telemetry budget and age rule of its own.** Rejected: ADR-0144 already decides what is
  kept and when it goes.

## Decision

Telemetry is one component, written through one leaf crate, `review-telemetry`, in its own store
under the state root's `telemetry/`. These eight clauses bind every package of the design.

1. **The kernel's execution authority never reads Telemetry.** No code path that decides a state
   transition, an identity, a replay, a Ledger entry, a budget reservation, an admission or a
   Worker input opens a telemetry record. Storage housekeeping under ADR-0144 — the inventory,
   the sweep, the free-disk floor and the `storage_sweep` record — reads Telemetry's structural
   metadata and nothing else: file sizes and times, lock state, `segment` headers and `seal`
   records, the `.refs` manifest of a sealed segment, the `blob` and `blob_ref` records in the
   valid prefix of an unsealed one, and the index's size. Its reader module (`segment::reader`,
   `index`, `status`, `blob::reader`, `blob::collect`) is linked only by `af telemetry`,
   `af storage` and the TUI; `review-process` links nothing of it.
2. **Telemetry is never a Worker input.** Nothing it captures is composed into a prompt, a Warm
   Set, Worker Notes or an optimization source. Reasoning summaries and thinking blocks are
   recorded for the operator at `content` and reach nothing else.
3. **Telemetry never steers.** Under identical clocks and storage conditions the Store's event
   log of a run is byte-identical with Telemetry `off` and `content`. Telemetry changes no flag
   that alters what a harness or a model does; it may change only what a harness reports, and
   the design's §7.3 lists every such flag. Every variable it adds to a harness's environment is
   listed in the design's §7.6.
4. **Machine-only configuration.** `[telemetry]` joins `[self]` and `[storage]` in the
   machine-only tables: a directory, project or local `.af/af.toml` that carries it is reported
   and ignored. The environment form is `AF_TELEMETRY__<KEY>`. A value outside its bound refuses
   the load, naming the key.
5. **Nothing leaves the machine by default.** The default sink is the local store. An OTLP sink
   is configured explicitly, pushes after the result is printed, is bounded in time and never
   changes an exit code.
6. **Credentials are withheld at every level, and identities survive.** One redaction
   implementation, the rules review-runner applied to Provider-authored text moved into the leaf
   crate and re-exported by review-runner, judges every untrusted string whole before any
   truncation; a line or document too long to be judged whole is withheld whole, never cut. JSON
   is redacted key-aware: a member named like a secret loses its value, and an object holding a
   member whose name itself passes for a credential becomes the string `[withheld]`, so every
   document still parses. A record attribute's name is judged by the same rule: one that passes
   for a credential is never written, the attribute is omitted and counted in `af.redacted`, and
   the record's other attributes stay; one named like a secret keeps its name and loses its
   value whatever its shape, as a JSON member does, and so does a command-line option's value;
   a configuration key named like a secret or passing for a credential withholds its whole
   pair, value and origin. The identity exemption holds only for a value that is exactly one of
   the kernel's identity grammars (a `sha256:` digest, a Task's `task:` run id, a lowercase-hex
   id of 16, 32 or 64 digits, a segment id, an occurrence id `segment_id:seq`, a decimal
   ordinal) in one of the closed set of identity positions the crate declares (`af.run_id`,
   `af.task_id`, `af.attempt_id`, `span_id`, `parent_span_id`, `trace_id`, the digest fields
   and the like); every other string, an identity-shaped one handed over as an identity
   included, is judged whole as untrusted text. Exact token and charge counters are canonical
   decimal strings only, any other value under their names (a number, text, any other shape)
   dropped and counted before any other rule. Every other
   field of every record kind is judged whole, and a JSON source longer than its bound, or a
   string array whose encoding is, is withheld before it is parsed or encoded; environment
   values are never recorded; Provider
   grants and the home prefix are replaced by placeholders, and a home prefix the environment
   does not give is recorded as a problem, never silently skipped. An occurrence keeps the
   identity of its original source bytes, its media type and its redaction version; the shared
   object keyed by digest holds only what is intrinsic to the retained bytes. A harness's untrusted content, from its streams
   or its native telemetry, is published only after Telemetry's own whole-capture release
   decision, with kernel-derived correlation ids standing in for the harness's identifiers until
   then. The rules carry a version that every blob records: `af-redact/1` at this decision,
   `af-redact/2` since the security audit's rule change.
7. **Telemetry fails open for the run and closed for the reader.** A telemetry-only failure (an
   unwritable or overlapping directory, a full segment, a locked index) is recorded once as a
   problem and the run continues; ADR-0144's free-disk floor still refuses work, telemetry or
   not. A segment, record, index or directory written under another schema version is refused by
   the reader and never migrated; so is a segment with an invalid line before a valid one, live
   or sealed, and a line over the record bound. Only an index lock not had in time sends a
   reader to the segments directly. A reader opens blob files only with `gc.lock` held shared,
   and defers verification without it. A recovered seal never claims what it cannot know: a
   writer's counts lost with the writer are `null`, the capture incomplete, and a writer's seal
   stranded before compression is superseded, never shown as a clean exit.
8. **Bounded disk, under the budget's rules.** Every physical byte under a telemetry directory
   is counted once in the Storage Budget as the kind `telemetry` (`af telemetry status` accounts
   each one, a path that matches no known kind under an `unrecognized` entry it never reads or
   removes), and evicted under ADR-0144's
   rules only: by sealed segment, least recently used first, never an entry in use or used within
   the last hour, by age only under `[storage] auto_gc` or an applied prune. A live segment has a
   total byte limit of its own with a fixed terminal reserve for its seal. Collection never
   leaves the telemetry directory: it acts only on names of the layout's exact grammar that are
   real directories and regular files, relative to the opened directory, never through a
   symbolic link, and readers never open a file through one. Beneath a telemetry directory the
   only pathname operation is its first open: a writer, `status` and `reindex` hold the
   directories they opened for their lifetime and reach segments, blobs, locks and the index
   database (with its WAL and shared-memory files) through those descriptors, recovery and
   direct reading included, so a directory renamed and replaced, by a link or by another
   directory, after it was opened takes nothing outside it. The index database is opened
   through a path naming the held directory itself, never its pathname, and refused where the
   file system offers none. Every directory
   Telemetry has written is registered (`af/TelemetryDirs@1`), and a directory that overlaps a
   Store, a cache, a release installation or another telemetry directory is refused before its
   first write; when the Store registry cannot be read the overlap is unknown, never assumed
   absent: registration refuses and `af telemetry status` reports the check `unknown`.

## Consequences

- The first package ships the crate (records, the locked segment writer, recovery, blobs with
  the publication protocol, structured redaction, levels, the `tracing` subscriber, identities,
  the directory registry, the index), the `[telemetry]` table, `af help telemetry`,
  `af telemetry status` (`af/telemetry-status@1`) and `af telemetry reindex`
  (`af/telemetry-reindex@1`). Later packages add capture; none adds an ADR.
- review-runner's credential rules now live in `review_telemetry::redact`; `review_runner::cli`
  and `review_runner::native_failure` re-export them under their old names, and their tests run
  unchanged against the moved code.
- Each clause is pinned by a test named in the design's §16; a change that weakens one needs a
  new ADR.
