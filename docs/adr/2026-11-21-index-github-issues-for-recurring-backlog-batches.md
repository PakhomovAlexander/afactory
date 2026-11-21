# ADR-2026-11-21-index-github-issues-for-recurring-backlog-batches: Index GitHub issues for recurring backlog batches

**Status:** proposed (2026-11-21).

## Context

[Recurring enrollment](2026-11-21-enroll-recurring-project-backlog-processing.md) freezes
intake without an operator per occurrence. The
[manual provider index](2026-11-20-bind-private-provider-observations-to-backlog-assessment.md)
requires installing a new batch pin by hand. A human's open GitHub issue may already cover a
report in the next frozen batch.

## Considered options

- Reuse a rolling private snapshot. A retry could assess different evidence for the same
  membership, and successive batches could miss new human issues.
- Read during the native Task. The read would require additional Worker authority and an
  independent verifier could observe a different issue state.
- Let the receiving scheduler acquire and bind one observation before preparing the native
  Scenario. Both Workers use existing private evidence contracts.

## Decision

The receiver opts in for a configured GitHub destination by confirming a version 2 enrollment
that pins the owner-only observer executable. The observer uses a receiver-local Issues-read
credential separate from the publisher. The scheduler executes only the pinned bytes with a
bounded lifetime and validates the returned observation against the existing repository and
input limits.

While holding the exclusive Project Store lock, the scheduler validates that the complete
observation fits the frozen collection. It then durably installs its exact bytes by content
digest and an immutable pin at a batch-specific path. Preparation requires that pin. First
settlement recomputes the collection against the same pin, and later settlement and publication
reconcile their existing receipts. An unbound batch may retry acquisition after a refused read.
A bound batch requires its original evidence.

## Consequences

Successive indexed batches use fresh observations without re-enrollment. An invalid read blocks
preparation and leaves the batch private. Manual pinning and the other destinations keep their
existing observation requirements. The live walk can detect incomplete responses and does not
provide a point-in-time snapshot of GitHub.
