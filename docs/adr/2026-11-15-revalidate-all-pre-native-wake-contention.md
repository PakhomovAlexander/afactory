# ADR-2026-11-15-revalidate-all-pre-native-wake-contention: Revalidate wake contention

**Status:** proposed (2026-11-15); repairs for #937 and #938, live certification pending.

## Context

The earlier [contention decision](2026-11-15-defer-only-checked-pre-native-wake-contention.md)
missed two ordinary writer windows. A checked writer could advance history inside lifecycle
projection, before the outer observation check. A public run or cancellation could also change
selected recorder files during page reconciliation. Both surfaced as terminal Pi subprocess errors
before native intent, ending an otherwise eligible finite attachment.

The cumulative review retains the coordinator-specified comparison base
`1a4dd4cc82d5f01f55014283c95d3bf6ea77fac3`. This repair continues the supplied candidate.

## Considered options

- Classify diagnostic strings or all conflicts as transient: rejected; conceals invalid authority,
  corruption and persistence failures.
- Extend polling or resend after native intent: rejected; changes finite bounds and uncertainty.
- Recognize only explicit logical contention, then validate current evidence together: selected.

## Decision

Use a nonserialized logical-change signal for inner projection, outer observation, checkpoint
replay and transactional page compare-and-swap failures. The journal alone converts that signal
into bounded deferral after a serialized validation of the current checked Task, exact opening and
subscription identity, expiry/closure, retained checkpoint and bounded selected recorder snapshot.
Recorder parsing and cancellation/process/checked-lease association validation must finish even
when the original snapshot changed. A process-only recorder may precede its writer's lease; it
supplies no cancellation or lifecycle authority. Invalid selected data remains terminal.

Page commit checks source and subscription authority before recorder comparison, and keeps both
recorder checks inside its transaction. Failed validation rolls back the candidate page. No stale
checkpoint, envelope, transport claim, native intent or uncertainty fence is created on deferral.
The authenticated Pi response rechecks its route and installation and uses the existing empty
response, timer, wall time and 128-poll bound. There are no persisted shape or schema changes.

The implementation report in the [wake design](../design/task-wake-journal.md) maps each boundary
to its typed handling and deterministic regression evidence. Debug barriers decide interleavings;
no sleep decides which writer wins.

This retains accepted research Document
`sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`,
source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`.
Its common Store boundary and conservative native uncertainty recommendation are unchanged.

## Consequences

Legitimate pre-native contention consumes a bounded poll and permits later checked completion.
Unknown errors, invalid authority, damaged history/checkpoints/recorders and native uncertainty
remain terminal or fenced under existing contracts. Independent full-candidate verification and
Pi/Codex live certification remain coordinator work; Claude live tests remain deferred under #876.
No service, login, host inference, Task dispatch or publication is added by this repair.
