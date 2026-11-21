# ADR-2026-11-20-bound-backlog-assessment-and-repin-between-batches: Bound backlog assessment and repin between batches

**Status:** proposed (2026-11-20).

## Context

The [daily batch decision](2026-11-20-freeze-project-backlog-at-authority-cutoff.md) pins
membership and policy. Independent assessment needs a repository citation for each supported
report and sufficient tracked-item context to rule out duplicates. Capacity-deferred delivery
also needs a disposition when the installed policy changes.

## Considered options

- Increase Report citation limits. This weakens a shared bound and does not address fair
  admission or incomplete tracked-item context.
- Scan the tracked backlog in resumable slices. This would need to bind every comparison
  to the exact tracked revision and aggregate independent verification across slices.
- Bound admission by the citation budget, hold new positive claims on incomplete context,
  and let a tick repin policy between batches.

## Decision

This amends clauses 1–3 of the [daily batch decision](2026-11-20-freeze-project-backlog-at-authority-cutoff.md).
A batch has at most 64 members, the current Report repository citation limit, and retains the
serialized member bound, charged after JSON escaping for the portable Task template.
An oversized member carries bounded, marked excerpts of its facts and reproduction. The
tracked view uses the remaining encoded budget after members and the exact provider observation.
Preparation refuses a pinned observation that cannot fit intact.
On admission, deferred members join the tail of existing carryover
before the occurrence queues new arrivals. A recurring deferral yields to the work already
waiting on each retry.

An assessment with an incomplete tracked-item view may identify exact duplicates in the
supplied view. It cannot verify another revision for publication. Delivery can still proceed
for an unchanged revision with a previously approved decision.

A tick can repin policy for the same Project and intake authority when no batch is active.
The transition retains frozen history, queue order, intake cutoffs and the original token
reservations in the new rolling window. Future due times must follow the last committed clock.
Queued evidence permissions are recomputed under the new policy. The pin transition durably
resolves outstanding capacity-deferred deliveries as requiring reverification. Operators can
explicitly reconsider the associated items under the new policy.

## Consequences

The journal uses `BacklogBatchStored@3` and the batch audit uses
`af/backlog-batch-audit@3`. Each audit batch exposes the policy ID under which it was admitted.
Existing active work can finish under its original installed policy before a pin changes.
