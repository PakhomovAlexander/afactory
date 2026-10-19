# ADR-0151: Record telemetry in its own store, and never let it steer

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
   document still parses. Kernel-produced, validated identity fields and counters are typed
   fields the heuristic never sees; environment values are never recorded; Provider grants and
   the home prefix are replaced by placeholders. A harness's untrusted content, from its streams
   or its native telemetry, is published only after Telemetry's own whole-capture release
   decision, with kernel-derived correlation ids standing in for the harness's identifiers until
   then. The rules carry a version, `af-redact/1`, that every blob records.
7. **Telemetry fails open for the run and closed for the reader.** A telemetry-only failure (an
   unwritable or overlapping directory, a full segment, a locked index) is recorded once as a
   problem and the run continues; ADR-0144's free-disk floor still refuses work, telemetry or
   not. A segment, record, index or directory written under another schema version is refused by
   the reader and never migrated.
8. **Bounded disk, under the budget's rules.** Every physical byte under a telemetry directory
   is counted once in the Storage Budget as the kind `telemetry`, and evicted under ADR-0144's
   rules only: by sealed segment, least recently used first, never an entry in use or used within
   the last hour, by age only under `[storage] auto_gc` or an applied prune. A live segment has a
   total byte limit of its own with a fixed terminal reserve for its seal. Every directory
   Telemetry has written is registered (`af/TelemetryDirs@1`), and a directory that overlaps a
   Store, a cache, a release installation or another telemetry directory is refused before its
   first write.

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
