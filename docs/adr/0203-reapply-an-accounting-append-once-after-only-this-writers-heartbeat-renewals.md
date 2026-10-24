# ADR-0203: Reapply an accounting append once after only this writer's heartbeat renewals

**Status:** proposed (2026-10-24). Narrows the residual live-owner sequence race of
[ADR-0128](0128-renew-a-live-task-writer-lease-through-its-own-connection.md).

## Context

The reserve heartbeat can renew through its own connection while a Task operation holds the
shared Store. An append validated against the earlier prefix then fails its SQL sequence fence.
A failed settlement leaves its started Attempt pending; Review conclusion correctly refuses it.
The reported overhead-smoke failure did not retain which Attempt or its scheduler diagnostic.
An ordinary local predecessor smoke passed; this does not prove the reported incident's cause.

The deterministic `renewal_race` regression recreates the accounting failure using an exact
checked projection followed by a real second-connection renewal before the append. The source
before this change refuses that settlement with its reservation still outstanding.

A second deterministic regression cancels Review at the committed `ContextBound` record,
before the scheduler's worker thread takes the prepared capability. Previously that unstarted
reservation remained pending, reproducing the conclusion refusal without any heartbeat race.
The test retains public accounting for that exact Attempt and the original scheduler report.

## Considered options

- Extend leases, ignore pending Attempts or settle them as successes: rejected; those changes
  erase authority loss or invent an outcome and accounting.
- Rerun a Worker or retry all Store conflicts: rejected; neither proves that paid work or
  policy authority remained the same.
- Retry after proving the intervening suffix contains only this owner's renewals: selected,
  once, with full fresh projection and transition validation.

## Decision

After a SQL sequence/lease comparison conflict, an ordinary execution-record append may be
reapplied once only when at most 64 intervening records are all `LeaseRenewed` by the exact
same writer and epoch. Read that suffix under row, row-byte and total-byte bounds. Re-project
and revalidate all references, the transition, the current lease and budget at the current
Store time, then obtain a new exact SQL prefix fence. The second conflict is returned.

No callback or external work repeats. No owned completion, canonical Review prefix transaction,
lease transition, takeover, release, changed usage, settlement or other state mutation qualifies.
An expired or successor-fenced writer still fails, and an interrupted owner leaves its Attempt
for the existing successor recovery path. No artifact/event shape or budget changes.

When dispatch fails before taking a prepared capability, the runtime releases that exact
unstarted reservation through the ordinary fenced Store API and retains the scheduler failure.
It never releases a started Attempt, creates a success, drops spend or retries work. A failed
release remains an error and successor recovery still owns lost-lease work. The Review regression
checks zero paid dispatch, released reservation, replay and an incomplete/inconclusive conclusion.
It establishes a real failing completion path, not the cause of every earlier incident.

The accounting fixture retains its three-second lease and renews at each setup/operation
boundary. Its slow-setup test spends more than a lease on multiple bounded steps, then tests
real expiry, successor recovery and old-writer refusal. Review's outstanding-Attempt refusal
includes the public pending IDs and scheduler-report IDs for investigation.

## Consequences

A proven renewal-only race need not strand an already admitted accounting record. Other
conflicts remain errors and retain their history and spend. This addresses the deterministic
race; it does not establish that every earlier intermittent lease conflict had that cause.
The full sealed-source gate and independent reviews remain required for acceptance.
