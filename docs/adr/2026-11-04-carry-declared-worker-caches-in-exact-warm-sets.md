# ADR-2026-11-04-carry-declared-worker-caches-in-exact-warm-sets: Carry declared Worker caches

Status: proposed; accepted on merge.
Date: 2026-11-04.

## Context

Issue #585 adds immutable cache reuse and explicit continuation to the private development
backend of ADR-2026-11-03-worker-development-environments. A Worker package cannot make a
candidate-built output an administrator-approved dependency or an authoritative Gate receipt.
Retry inputs must remain exactly those selected before reservation, including after a crash.

## Decision

`af.worker-development/2` declares bounded cache roles, symbolic destinations, private subpaths,
compatibility fingerprints and keys, optional input ports, explicit Task-scoped lifecycle and
admitted-output persistence. Immutable bytes follow Task retention and collection; writable
clones remain Attempt-private. Other lifecycle requests fail closed.
`af.worker-development-binding/2` grants per-cache ceilings and names approved dependency
Manifests in the existing CAS. Acquisition remains `available_only`: a separate trusted operator
operation imports credential-free dependencies. The runtime never acquires from a Worker,
fetches a dependency or maps an operator HOME or writable host cache.

Dependency Cache Snapshots are approved immutable Manifests, cloned privately and never written
back. Candidate-built caches use `review.kernel/BuildCache@2`, with a language-neutral `worker`
kind, Task/slot/name/role provenance, producer Attempt and initial/final tree digests. The
existing `gate_node` and `gate_attempt_id` fields identify the producer for this kind. Gate
captures retain their existing Gate-only event and trusted-local, same-Round handoff rules.
Worker caches have no route into that handoff or the Gate's warm-check directories.

The compiler permits Worker-cache edges only between Worker nodes on the same effective slot.
A cache output is kernel-constructed and omitted from the Worker reply contract. It becomes an
ordinary Attempt-produced typed output only after the reply, source seal and durable output
admission succeed. Captures of a failed, abandoned or fenced Attempt are retained evidence,
never continuation authority. The optional `development_warm` input is reserved to the kernel;
no Pipeline may bind it and no Worker may author a Warm Set.

The common Task host selects `review.kernel/WarmSet@3` before recording an invocation and before
reservation. Each declared cache has either exact cache/Manifest/compatibility identities or a
bounded omission reason. The invocation pins this artifact. A retry or restarted runtime uses
that recorded selection, never a new query. A selected object lost or corrupted afterward
refuses restoration; an initial miss, incompatibility or corruption is an explicit cold outcome.
WarmSetSelected takes version 3 for the expanded layer vocabulary. Previous versions of these
changed contracts are unsupported, with no migration or fallback reader.

Compatibility hashes the pinned request and machine binding (including image/tool identities),
host OS/architecture, declared flags/profile/features keys and bounded source-file identities.
Missing fingerprints have an explicit identity distinct from empty files. Build captures use
final source fingerprints, so producer edits to a dependency lock cannot be attributed to the
initial lock. Compatibility grants reuse only; it never asserts a command or test passed.

Continuation state additionally requires the exact source Snapshot and final tree digest. Only
an explicit state subdirectory may persist; HOME and configuration remain fresh, and neither
source nor conversations are implicitly restored. Notes and knowledge remain separate;
Session Snapshots remain unsupported under ADR-0145.

The shared closed Build Cache walker and CAS publisher enforce byte, entry, depth and path
limits, no-follow descriptors, regular files only and fixed private metadata. Traversal and
streaming honor the Attempt deadline and cancellation. Captures publish an immutable Manifest
only when complete; a failed clone removes its partial destination. Captures have versioned
success/omission receipts. CheckRuntime owns writable clones and existing preservation rules
hold whenever container cleanup is unconfirmed. CAS publication barriers, live Task leases,
reachability accounting and bounded Task collection own persistence and crash recovery; there
is no second cache index, writable cache lifetime or GC owner.

## Considered options

- Shared host targets or operator HOME mappings: rejected because sibling isolation and
  dependency approval would depend on mutable directories.
- Select the newest compatible output on every retry: rejected because recovery could silently
  change admitted inputs or inherit a failed sibling's state.
- Let a Worker emit its own cache artifact payload: rejected because a claimed Manifest or
  provenance is not a kernel capture of its actual private root.
- Add a separate cache service and lease protocol: rejected because existing CAS, Task leases,
  CheckRuntime and collection already own the required lifecycle.
- Restore arbitrary state or harness sessions: rejected because diagnostics are not source or
  conversation continuation authority; the later knowledge step is separate.

## Consequences

Workers without cache declarations retain Attempt-private roots. Reuse is explicit within a
Task's same-slot graph, with no ambient cross-Task lookup. Machine policy can lower every cache
bound or grant no persistence. Gate acceptance and independent reviewers retain their existing
authority. The design and examples describe the new versions and do not claim knowledge or
session carry.
