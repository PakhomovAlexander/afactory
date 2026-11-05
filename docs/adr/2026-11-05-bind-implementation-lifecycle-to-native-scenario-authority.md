# ADR-2026-11-05-bind-implementation-lifecycle-to-native-scenario-authority: Native impl lifecycle

**Status:** proposed (2026-11-05).

## Context

A reusable implementation workflow must stay active until a human merges its PR and its owned
workspace is removed. Task acceptance, CI success, publication, merge and cleanup are distinct
facts. Existing Scenario publication grants do not authorize materialization, commit, push or
workspace deletion. A separate verification Task unnecessarily repeats acceptance machinery when
a required reviewer can execute checks against the same candidate.

## Considered options

- Put arbitrary shell in Scenario definitions: rejected because repository text would acquire
  machine credentials and arbitrary filesystem authority.
- Treat green CI, a successful publication process or a background merge watcher as completion:
  rejected because none establishes native merge and cleanup receipts.
- Change existing pinned Tasks or grant execute-checks to every reviewer: rejected because
  authority belongs to the exact consuming package and captured plan.
- Retry PR creation after an uncertain response or overwrite a changed branch: rejected because
  service absence does not prove no effect and foreign changes must be preserved.

## Decision

Introduce native typed `impl_prepare_v1`, `impl_publish_v1` and `impl_cleanup_v1` Actions. They
require separately installed, digest-pinned machine authority and exact repository scope. Prepare
consumes an accepted implementation Task receipt and native source export, stages raw Snapshot
bytes, and records deterministic commit bytes before publication. Publish requires the existing
exact native signed Decision, binding prepared content and commit. It permits only initial owned
branch creation/PR publication or an explicitly signed fast-forward continuation on the same PR.
It never merges. Cleanup requires exact typed merge evidence and fresh real merge observation.

The reusable builder creates consuming-repository packages with one implementation Task and an
executable correctness reviewer, both reviewers required, an independent evaluator and local Gates.
All four model bindings use codex-personal and the owner defaults documented in the operator guide.
The native planner checks that effective contract. The reviewer receives the applied candidate,
privately builds/installs/tests it, and cannot seal source changes. Missing execution evidence
blocks acceptance. A required versioned public ImplementationTestEvidence artifact travels beside
the unchanged flat Reviewer Result and into the evaluator's separately versioned input contract.
Native admission requires successful build, install and test records for a passed testing status;
failed, unavailable or stale testing cannot support a passed evaluation. No separate verification
Task is part of this Scenario.

Allow at most two bounded implementation continuations on accepted predecessor candidates after
terminal exact-head CI/review/conflict evidence. Each consumes its own finite reservation and
requires a new exact publication signature. Typed selection resolves only the selected available
receipt; it cannot alias a signed Decision. Unresolved work never completes. Newly changed remote
base bytes are not admitted by this implementation; conflicts requiring those bytes remain blocked
instead of manufacturing a resolved candidate.

The installed adapter retains write-ahead intents, native preparation provenance, PR identity,
branch leases and worktree/build ownership. Reconciliation precedes mutation. The new adapter request generation adds an explicit `recover`
method only for retained dispatched native lifecycle intents, under their original exact inputs,
grant and signed authority. Reconcile and observe execute no remaining lifecycle substep. Recovery
continues only durably reserved substeps and never treats an uncertain POST as absent; uncertain PR POSTs
are never resent on negative evidence. Changed heads invalidate observations. Closed unmerged PRs
retain their workspaces. After actual human merge, cleanup preflights every explicit owned lineage
member, preserves dirty/foreign/replaced artifacts, removes only owned worktrees/builds and deletes
branches only at the exact expected commit. Completion requires both merge and cleanup.

## Consequences

Scenario, bundle, policy, pin, Run, list and Ant wrappers advance to generation 4; the Scenario
Store advances to 5, adapter requests to 3 and embedded GitHub request journals to 3. Their readers,
writers, schemas and fixtures change together; earlier generations fail closed without migration.
New implementation preparation, outcome, source request/receipt, machine configuration and adapter
state each have their own generation 1 contract. Existing Task packages and Task delivery remain
unchanged. No accounts, scheduler, remote Gate or production instance are installed by the Worker.

Partial checkouts and native captures remain preserved when recovery cannot prove their identity.
The operator guide states this limit and the unresolved changed-base conflict path explicitly.
See [Implementation through human merge](../scenario-impl.md) for installation, signing, bounds,
recovery and the executable test matrix.
