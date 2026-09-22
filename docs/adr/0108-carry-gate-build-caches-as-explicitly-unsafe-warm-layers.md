# ADR-0108: Carry Gate build caches as explicitly unsafe warm layers

Date: 2026-09-17
Status: Accepted; superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): the
legacy Kernel's in-memory record of a Worker's clone measurement.

Implements package P2 of [`docs/design/worker-warm-layers.md`](../design/worker-warm-layers.md)
on top of [ADR-0107](0107-carry-worker-notes-and-head-deltas-as-declared-warm-layers.md) (the
Warm Set), [ADR-0008](0008-safe-caches-are-sandbox-local-snapshots.md) and
[ADR-0036](0036-resolve-gate-caches-through-machine-local-bounded-policy.md) (Cache Snapshots
and the reserved `.af-cache` layout), [ADR-0028](0028-prioritize-wise-token-use-and-minimum-worker-context.md)
(every carried layer is declared in the manifest) and
[ADR-0002](0002-event-payload-changes-bump-the-type-version.md) (new events, not widened payloads).

## Context

The Gate builds the head before any reviewer is dispatched, then throws the build away. A TDD
reviewer or an implementer rebuilds the same head minutes later in its own sandbox. The safe
cache path that rc.3 ships carries only an administrator-approved, credential-free registry
snapshot (`CacheManifest@1`, `.af-cache/cargo`, `CARGO_HOME`, offline), because those bytes come
from machine policy and never from candidate code. Build output is different in kind: it was
produced by candidate code under a policy that can read anything the operator can, so nothing
about it is administrator-approved, and the design's review Finding `cd4f02d1adf5` requires that
it never be typed, named or documented as a Cache Snapshot.

## Decision

- **One cache mechanism, two trusts.** A build cache lives below the same reserved `.af-cache`
  root as a Cache Snapshot, is traversed with the same descriptor-relative no-follow discipline,
  gets the same fixed private modes and stripped extended attributes and ACLs, and is removed by
  the same `remove_materialized_caches` before any seal. There is no second cache mechanism and
  no second selection file. What differs is trust, and trust is stated in the artifact.
- **`review.kernel/BuildCache@1` is explicitly unsafe.** Its payload carries
  `trust: candidate_built`, the closed `kind` (`cargo_target` today), the Gate node, the common
  Task Attempt the Gate ran under when the Task runtime executed it, the head Snapshot it built,
  a source `Manifest` of `file` and `executable` entries whose bytes are CAS objects, that
  manifest's Tree Digest, the entry and byte counts, and the limits the capture applied. The
  closed capture layout admits regular files only, refuses a symlink, FIFO, socket, device or
  credential-shaped path, and bounds entries (directories included), depth, path length and
  bytes below kernel ceilings. A stored manifest is re-checked against the same layout before it
  is cloned, because a stored artifact earns no more trust than a fresh capture.
- **`cargo_target` is a closed kind.** It points `CARGO_TARGET_DIR` at the sandbox-local clone
  and nothing else. The registry-only `cargo` Cache Snapshot under `[gate] caches` keeps its
  meaning unchanged; `[gate] build_caches = ["cargo_target"]` is a separate declaration.
- **Trusted-local only, refused three times.** `build_caches` is accepted only on a
  `trusted_local` Gate binding with `required_isolation = "none"`; a container or otherwise safe
  binding is refused at load. A reviewer's `warm = { build_cache = ["cargo_target"] }` must name
  a kind its Gate declares, or the pipeline is refused at load. At runtime the same policy is
  re-checked at Warm Set selection, before the node's first Attempt is reserved or dispatched,
  and again at every capture and clone.
- **Typed Gate-to-Worker handoff within one Round.** After its checks pass, the Gate captures
  each declared kind and records `BuildCacheCaptured@1` on the Gate node with exactly one of the
  artifact or a refusal reason (`unsafe_content`, `limit_exceeded`, `source_unavailable`,
  `capture_failed`), plus the head and the limits applied. The record is published in the same
  batch as the Gate's `GateDecision@1`, so no log holds a capture whose decision did not become
  durable with it. A refusal never changes the Gate verdict; the Workers that declared the kind
  run without the layer. A reviewer that declares the kind must be `gated_by` the Gate that
  captures it, refused at load otherwise; Warm Set selection reads only the record that Gate
  published before its passing decision, never another Gate's record or one drained later
  from a Gate Attempt that never decided, and records `build_cache_artifact_id` or a
  `build_cache_dropped` reason in `WarmSet@1`; the `build_cache` layer joins `WarmSetSelected@1`.
  A node that declares a build cache kind therefore selects a Warm Set in every Round, including
  Round one, because this layer travels from the current Round's Gate rather than from the
  previous Round's Attempt. Notes and the Head Delta keep ADR-0107's rules.
- **Cloned per Attempt, removed before seal.** Every Attempt of the node, retries included,
  receives a fresh clone of the same artifact from the CAS into its own sandbox, and the adapter
  receives the pointing variable as sandbox-local environment that is never serialized, never
  rendered and never durable. The context manifest lists the layer with its artifact identity
  and zero rendered bytes. The bytes are removed before the sandbox is sealed, so a warm
  Attempt's sealed diff is byte-identical to a cold run's and no build output can enter a
  candidate tree, a Proposal or a delivered worktree.
- **Evidence, not a cache-hit claim.** Capture and clone are measured as
  `dependency_preparation` spans and cache observations in the existing `TaskRuntimeEvidence@1`
  shapes, keyed by the Gate's kind. They record bytes made available and host time; they never
  assert that a compiler reused anything. A Task-hosted Gate settles its capture with its checks;
  a Task-hosted Worker settles its own clone as its own `TaskRuntimeEvidence@1`, after its raw
  reply, so `af task show` reports what each Attempt's preparation cost. The legacy Kernel keeps
  the measurement in memory beside the node, as it does for check spans.

## Considered options

- Pass the Gate sandbox's target directory through to reviewer sandboxes as a host path:
  rejected, that is the passthrough ADR-0008 forbids and leaves nothing to replay.
- Widen `CacheManifest@1` and `CacheSnapshotMaterialized@1` with a `cargo_target` kind: rejected,
  it would give candidate-built bytes the Cache Snapshot name and the administrator-approval
  guarantee that RunReport@5 readers rely on.
- Keep the captured tree in a kernel-owned temporary directory for the Round: rejected, a
  resumed Round would find nothing or, worse, find a directory a previous process left behind;
  warmth is an artifact, so the bytes are CAS objects addressed by a manifest.
- Admit the handoff under the safe policy with extra scanning: rejected, no scan makes
  candidate-built bytes administrator-approved; the safe policy keeps its registry snapshot.
- Let the reviewer declare the kind without the Gate declaring it: rejected, the Gate is the
  producer and its declaration is what bounds the capture.

## Consequences

- A TDD reviewer or implementer on a trusted-local pipeline starts from the Gate's build. The
  dogfood comparison the design requires ships with the first warm Campaign, not this package.
- Build caches are bounded CAS growth: the declared or default byte limit per Gate per Round,
  addressed by manifest so duplicate content costs nothing. Collection of unreferenced objects
  remains a separate concern.
- One new event type and one new artifact type join the closed vocabularies; `WarmSet@1` and
  `WarmSetSelected@1` gain optional fields. Existing logs replay unchanged, pipelines without
  `build_caches` and `warm.build_cache` are byte-identical to before, and the self-optimizer cache
  path and its fixtures are untouched.
- Claude reviewers keep ADR-0042's read-only tool set, so the variable is inert for them until a
  package grants a build tool; Codex reviewers and command Workers use it immediately.
