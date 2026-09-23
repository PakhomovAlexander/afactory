# ADR-0084: Route new Review commands through the common Task

Date: 2026-09-12
Status: Accepted (2026-09-23); superseded in part by
[ADR-0113](0113-ga-reads-only-what-ga-writes.md): the original executor for historical paid
Campaigns, and historical readers and output generations. `af review run --json` always emits
`af/review-outcome@3`.

## Context

The captured Review compiler, domain host and common runtime now preserve Review operations,
owned slices, Provider admission, heavy continuation and post-Round Integration. The installed
CLI must use those boundaries without creating a second scheduler or reinterpreting historical
paid Campaigns. Interrupted preparation and a published successor Round must remain recoverable.

## Decision

New `af review run` and `af provider doctor` executions capture one Review Task, addressed
by a domain-separated digest of the Campaign identity. The installed plan retains exact captured
Round inputs, policy, dependencies, model bindings and original resources. Only unpaid source
and Campaign preparation can enter this path without an existing Task. Historical paid
Campaigns retain their original executor and accounting. Common Review evidence with missing
Task state refuses; it cannot fall back to a fresh legacy allowance.

Capture the lifetime resource envelope once with the installed compiler policy. For new work
whose existing policy has no Attempt token cap, use a 300,000-token fallback. Captured explicit
caps take precedence. Bounded retries, owned fan-out, Provider probes, Review Rounds and optional
Integration determine the lifetime envelope. An unknown interrupted Command Attempt retains
its conservative original reservation even when successful Command calls normally report zero
tokens. Resume, source epochs and later Rounds never renew resources or the absolute deadline.

Restore execution from recorded CAS authority, recompile the exact plan and validate current
local Provider identity before obtaining the writer lease. Recover disappeared Attempts under
the new lease before resuming dispatch. Selected outputs replay through the common runtime;
completed Gates and Provider probes are not repeated. Generated-plan admission rules remain
part of the common Store boundary.

Prepare successor source and artifacts under the existing lease heartbeat without holding the
Store mutex across Git or CAS work. A protected publication permit binds both event-log prefixes.
After preparation, obtain a fresh permit and require an identical prospective Review history.
Validate the exact successor plan and its feasibility under the original remaining allowance
before atomically publishing the canonical Round. A crash before Task handoff recovers that
recorded successor and admits it without recapturing source or starting another Round.

Provider doctor selects only the exact captured Provider admission operations, in graph order,
using normal invocation, reservation, execution, output and heartbeat boundaries. It emits a
Provider admission summary, not a partial business run report or a Task result. A following
Review uses the same Task and durable probe output. Successful historical capability remains
factual after a later resource breach, while current doctor readiness becomes false.

New CLI JSON uses strict `af/review-outcome@2` and `af/provider-doctor@2`. Decimal strings carry
wide cumulative counts. Selected reviewer context and usage are separate from the whole Task
budget, which includes retries, probes, earlier Rounds and late observations. Canonical Review
verdict and overall Task completion are separately visible: missing required work or exhausted
resources cannot become a clean outcome merely because an earlier Round passed. Existing
machine-readable generations and historical readers remain supported.

Prepare output while the writer lease is maintained, then release the lease successfully
before emitting it. Slow or blocked stdout must not turn a printed success into a lease error.
An attempted but unsuccessful doctor admission emits one typed doctor document and returns a
nonzero exit; setup and authority failures retain the ordinary pre-admission error contract.

The common report/ledger views derive factual timing from durable Attempt sidecars and Review
Round boundaries. They do not add cumulative report snapshots together or copy common charges
into legacy per-Round accounting. Read-only inspection does not modify either event log.

## Considered options

- Dispatching directly in the CLI would duplicate reservation, recovery and Provider decisions.
- Migrating paid historical execution would require changing already-captured accounting.
- An uncapped common Task would defeat bounded admission; recapturing its envelope on resume
  would replenish spent resources.
- A doctor that ran an arbitrary graph subset could publish misleading business completion.

## Consequences

Task owns new Review execution while the canonical Campaign, Round, Finding, Demand and
disposition contracts keep their existing domain meaning. Real CLI fixtures cover Provider
reuse, original resource continuity, source restart, two-Round inspection and SIGKILL recovery
after actual lease expiry. Full checkpoint verification is recorded in the Task implementation
log; this decision does not claim specialist review, live pilot or release completion.

Frozen tree `910d188d8428dcebdb8b1026718c24bf01c6a984` passes the full local gate at
`32c0f4b`: 1,078 tests, zero failures, 15 ignored across 127 suites, formatting, Clippy,
documentation tests and byte-identical fixtures. Product `fdffb1c` has that exact tree.
Subsequent native usage and expired-Waiting recovery corrections remain separate work.
