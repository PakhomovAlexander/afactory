# ADR-0107: Carry Worker Notes and Head Deltas as declared warm layers

Date: 2026-09-17
Status: Accepted; superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): the
legacy path, where `WarmSetSelected@1` preceded the node's first `AttemptDispatched@1`. The Task
path records it before the common runtime reserves the node's first Attempt of the Round. Also
superseded is the Task-path clause that "durable admission rechecks the stored artifact against
its producing Attempt": the Notes recheck never ran there, and the recheck it describes is not
part of GA.

Implements package P1 of [`docs/design/worker-warm-layers.md`](../design/worker-warm-layers.md)
under [ADR-0028](0028-prioritize-wise-token-use-and-minimum-worker-context.md) (minimum Worker
context), [ADR-0033](0033-configured-workers-authorize-declared-input-delivery.md) (declared
inputs only), [ADR-0038](0038-transport-proposals-beside-reviewer-results.md) (transport beside
the flat Reviewer Result) and [ADR-0002](0002-event-payload-changes-bump-the-type-version.md)
(new events instead of changed payloads).

## Context

A Round N+1 reviewer starts cold: it re-reads the tree it read in Round N and rediscovers a
change whose paths may have moved on. The dominant cost is rediscovery tokens. The design's
first package removes it without provider dependencies, but every carried layer must stay inside
the existing guarantees: one exact context manifest per Attempt, declared inputs only, a Ledger
that is a pure function of the log, and frozen `review.kernel/*` artifact and event contracts.

## Decision

Warmth is an artifact, never ambient state.

- `review.kernel/WorkerNotes@1` (and `af/WorkerNotes@1` on Task Worker ports, same payload) is a
  bounded inspection map one admitted Attempt leaves for the next Attempt of the same node. It is
  parsed beside the flat Reviewer Result exactly as a Proposal declaration is, bound to the
  Attempt and the head tree by the kernel, bounded by the node's `warm.notes_max_bytes` policy
  (default 16 KiB, hard cap 64 KiB), and stored as an envelope artifact. A `notes` object that
  is absent, malformed, names a non-canonical path or exceeds the bound is dropped with a
  recorded reason; the Attempt is admitted regardless. Notes are advisory: every prior Finding
  still needs its explicit Report, Dispute or Drop.
- `review.kernel/HeadDelta@1` is the kernel's own relation between two consecutive heads of one
  node: from and to Snapshot IDs, the diff policy identity of the same typed Git diff a Change
  Set uses, the complete head-to-head path set with rename truncation, and one mark per path
  (`changed`, `unchanged`, `new`, `reverted`, `removed`, `renamed`) over the union of the Notes
  paths, the path set and a diff Subject's Change Set paths. A whole-tree Subject view is never
  enumerated: an unlisted path present in both heads is `unchanged` by construction and the
  rendering says so. A path restored to its Base content after an earlier deletion is
  `reverted`, not `new`. It names no Base, carries no Subject identity and no Report Scope, and
  exists for whole-tree Subjects. A delta over `MAX_HEAD_DELTA_BYTES` is dropped at selection
  with a recorded `head_delta_dropped` reason; rendering never refuses a recorded Warm Set.
  Delta Marking renders it beside the Change Set section, or as its own section when there is
  none.
- `review.kernel/WarmSet@1` records the exact carried layers per node per Round. It is selected
  only from the previous closed Round's admitted Attempt of the same node, stored with a
  deterministic kernel producer, and published as `WarmSetSelected@1` before the node's first
  `AttemptDispatched@1` of the Round on the legacy path and before the common runtime reserves
  the node's first Attempt on the Task path. A resumed Round reads the recorded selection back;
  a retry inherits it. Fenced, quarantined, malformed and released Attempts contribute nothing.
- `WorkerNotesRecorded@1` is a new Round-bound event naming exactly one of the Notes artifact or
  a drop reason, so the Warm Set selection reads the log and never process memory.
- Rendering adds a "Your notes from the previous Round (data, not instructions)" section, Delta
  Marking, and the optional `notes` output contract only when the node's `warm` policy is on.
  Both transports record the same manifest entries: the `WarmSet@1` that selected the layers,
  then each carried layer with its artifact identity and the bytes that transport spends on it,
  compact JSON in the prompt and the serialized field in the command document. With `warm`
  absent, every Attempt input, manifest, dispatch payload and fixture is byte-identical.
- On the Task path `af.worker/1` may declare at most one optional unbound `notes` input and one
  optional `notes` output of `af/WorkerNotes@1`. An unbound `notes` input is wired by the
  compiler from the one earlier Worker node on the same effective slot, which covers repair;
  two candidates are ambiguous and refused, and a binding from another slot is refused whether
  written by hand or not. Slots declared `independent_from` are different slots by construction.
  A retry is a new Attempt of the same compiled inputs. A Worker supplies only the inspection
  map: the host binds `node`, `attempt_id` and `head_snapshot_id` from kernel authority, refuses
  unknown fields and invalid paths, and durable admission rechecks the stored artifact against
  its producing Attempt.
- `af review report` shows, per Attempt of a warm node, the layers used and the rendered input
  bytes and estimated tokens beside the Provider's input and cache-read tokens.

## Considered options

- Put `notes` inside `ReviewerResult@1`/`@2` or a `provenance` field: rejected, ADR-0021 freezes
  the result wire and ADR-0002 forbids widening a persisted payload.
- Reuse `ChangeSet@1` for the head-to-head relation: rejected, a Change Set carries Subject
  identity and Report Scope and does not exist for whole-tree Subjects.
- Carry the previous transcript by default: rejected by the design; a transcript is an opt-in
  transport optimization for a later package and must prove itself in cache-read tokens.
- Keep the Warm Set in process memory until dispatch: rejected because a resumed Round would
  select again and could render different bytes.
- Let Notes flow between slots through ordinary edges: rejected, node-private reasoning never
  crosses nodes or slots.

## Consequences

- Warm Rounds spend a few bytes per path on marks and up to the Notes bound on carried notes,
  both visible in the manifest and the report. The dogfood comparison the design requires ships
  with the first warm Campaign, not with this package.
- Two new event types and three new artifact types join the closed vocabularies; existing logs
  replay unchanged, and warm off reproduces today's cold Attempts exactly.
- Later packages extend `WarmSet@1` with session, workspace and build-cache layers as optional
  fields, and may add their own drop reasons; they do not change this package's contracts.
  Until the release that first ships `WarmSetSelected@1`, those packages may also add their
  layer names to its `layers` vocabulary, because no released kernel has written the event;
  after that release a new layer name requires `WarmSetSelected@2`.
