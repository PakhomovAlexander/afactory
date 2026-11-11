# ADR-2026-11-10-reuse-validated-artifacts-for-task-observation: Reuse validated Task observations

**Status:** accepted (2026-11-10). Supersedes in part
[ADR-0096](0096-revalidate-task-execution-evidence-on-cached-replay.md): repeating typed
validation and projection construction on an unchanged observation. Its requirement to read and
verify every artifact in the observed closure remains. Also supersedes the benchmark workload
event mix in ADR-0163, ADR-0168, ADR-0170, ADR-0171 and ADR-0172; exact ordered history and
independent full-output judgment remain required.

## Context

An unchanged `af task wait` repeatedly reconstructed a Task projection and its display. Typed
artifacts were parsed, canonicalized and validated repeatedly during each projection, even when
the same operation had just validated them. The optional last Gate hint projected every Task in
the Store and retained their execution graphs together.

The investigation's overlapping projection, decode, canonicalization and verification profile
fractions explain where to inspect; they are not additive time savings. Its shared-host GNU and
installed musl observations are diagnostic evidence, not a calibrated comparison.

## Decision

1. Each open CAS keeps a process-local parse memo of at most 128 typed artifacts and 256 MiB of
   conservatively charged decoded allocations. The charge includes strings, arrays, object nodes,
   provenance and entry overhead. The 5.6 MB numeric-byte package uses about 192 MiB for
   its byte array as 72-byte ordered JSON Values; an untrimmed parsed allocation can reserve
   288 MiB. Validated misses trim spare array capacity before sharing the decoded envelope.
   A wire-size-only limit would miss this workload. An oversized entry is not retained; FIFO eviction bounds both
   bytes and entries. A mutex protects the memo, and immutable envelopes are shared with `Arc`.
   Callers retaining returned data own that memory independently of the memo.
2. The reuse key is the requested artifact ID and the fingerprint of the complete current raw
   bytes, using the content-domain SHA-256. Every access opens and reads the object again through
   bounded scratch. Size, mtime, inode, SQLite notifications and prior success are insufficient.
   A miss decodes the same handle and checks its bytes against the observed fingerprint before
   retaining a fully validated envelope. Failed reads invalidate the entry. Raw blobs keep their
   content-domain distinction; typed identity never comes from a JSON `type` key alone.
3. Canonical serialization writes to byte, comparison and buffered hashing sinks. Envelope
   identity and writing borrow the payload rather than cloning it into another JSON value. Numeric
   admission reuses its error-pointer buffer; already ordered objects need no key vector. UTF-16 key order,
   escaping, number refusals, domains, canonical bytes and the 8 MiB envelope limit stay unchanged.
4. A projection retains the union of its exact event-reference closure and successful CAS reads
   reached through validation. Nested reads contribute to their parent's closure; other threads
   and other CAS instances do not. An unchanged observation checks its Task watermark, re-verifies
   the entire closure, then checks the watermark again. Review handoffs and Integration also
   retain every replayed stream's prefix and check those watermarks before and after verification.
   A changed prefix takes ordinary full validation. Observations grant no execution capability.
   The waiter also retains and verifies artifacts its display read. It reconstructs the view on
   changes or lease expiry and continues observing startup run files. Cancellation, timeout,
   completion notification and exit-code rules stay unchanged. The 250 ms poll interval stays.
5. The last Gate hint discovers execution records through a SQLite partial index over existing
   authoritative event rows. No summary artifact or durable cache becomes authority. Discovery
   retains only the greatest matching check start time per Task as an upper bound. Competing
   Tasks pass full projection validation before their spans can supply the hint. Repository and
   code policy must match; the current plan must name the check. Greatest start time wins, with
   Task-label order and then recorded Attempt/raw/span order breaking ties as before. Collected
   Tasks are excluded. Observed corrupt evidence refuses the hint. Unrelated lease-only Tasks
   need no projection; ordinary Task listing still validates every listed Task.

## Considered options

- Metadata-only reuse or polling only on SQLite changes: rejected because an in-place CAS
  mutation or deletion need not change an event or trusted metadata.
- Poll backoff alone: rejected because it postpones observation and leaves the repeated work.
- Retaining every Task projection for history: rejected because memory grows with historical
  execution graphs. A persisted summary would also need a separate authority and replay design.
- Repeating canonical output buffers and whole-payload JSON conversion: rejected because sinks
  can preserve the same bytes without those allocations.

## Consequences

An unchanged wait still reads all observed artifact bytes; this optimization does not promise zero
I/O or constant cost independent of artifact size. A cold typed read may read bytes twice, and an
entry beyond the memo's limits is validated without reuse. New event suffixes still use ordinary
validated replay. No persisted artifact, event or public status document changes shape.

The full CLI benchmark retains the original 10,000-event fixture and adds controlled small and
artifact-heavy fixtures with 100 or 10,000 events and zero or 100 historical Tasks in the same
Store. All state-log fixtures record pauses and resumes before proposal, then take a fresh
production lease after admission. Every 64th setup pair is a live release/take handoff in place
of a pause/resume pair, preserving the event count without depending on one lease lasting the
whole setup. An expired release still fails immediately. The original two-node artifact workload and event count remain.
The clock-dependent renewal loop is replaced with exact pauses/resumes and lease handoffs under
`state_projection_v3`, `state_projection_seed_v2` and `cli_state_fixture_v2`; the full-output oracle
counts all six post-opening transition kinds within its eight-selector bound. `persistence_fs_v2` measures 100 release/take transitions after
a five-event prefix. Old workload versions are refused. This avoids requiring one advancing host
millisecond per renewal without changing production lease rules or adding waits or retries.
All writes use public APIs and execute no Worker. The primary
metric is total CPU from one reaped-child rusage record, user plus system; wall and RSS are reported
separately. No tolerance or resource budget increases. See the
[measurement procedure](../measurements/task-wait-validation-reuse.md).
