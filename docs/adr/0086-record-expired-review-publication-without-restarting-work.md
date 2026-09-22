# ADR-0086: Record expired Review publication without restarting work

Date: 2026-09-12
Status: Accepted (2026-09-23); superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): earlier
transition and inspection generations. Recording recovery is an ordinary change of
`TaskTransition@5`, the only transition, and `af/task-inspection@11`, the only inspection, shows
it in its history without a generation of its own.

## Context

A common Task can finish a paid Reviewer Attempt and persist its selected output, then fail to
publish the corresponding canonical Review fact. The Task pauses for domain publication.
Its original execution deadline can expire before recovery. Ordinary resume correctly refuses
expired work, but this also prevents recording the already captured Reviewer result. Extending
the deadline or rerunning the Worker would change the original execution authority.

## Decision

Add `TaskTransition@4` with `RecordingResumed`, bound to the exact Task revision, Execution Plan
and latest failed RunReport. This transition applies only to an originally admitted Task paused
from Running in `Waiting(NeedsHuman)` after its original deadline. Require a domain-publication
failure with an output already published and selected in the common Task at that report's exact
prefix. It must still be reusable; a successful settlement without publication is insufficient. Active Review
Integration and pending Attempts refuse this recovery route.

When projecting `RunReported`, verify its `through_sequence` against the current Task prefix and
retain the exact output map at that point. Derive the recovery capability from that report
snapshot. An output published between the report and the later pause cannot expand the set.
Reopening the event log reconstructs the same snapshot; no mutable local plan substitutes for it.

Require the current writer lease, captured plan authority, unexpired and unrevoked developer
plan decision where applicable, and exact canonical Review Round. Repeat the prefix, lease,
decision and Round checks inside the protected append transaction. Recording recovery does not
waive current authorization or permit a changed revision, plan, report or selected output.

The common Runtime may replay only the invocations and outputs pinned by this capability and
publish their missing canonical Review facts through a dedicated recording guard. The original
deadline continues to refuse every new invocation, reservation and business operation, including
pure work. Preserve original limits, deadline, Attempt identities, output identities and paid
usage. Missing work is reported under the original resource bounds.

A Task marked for recording recovery cannot complete as Satisfied. The factual recovery proof
finishes Inconclusive: retaining a captured result does not establish that the rest of its
Pipeline or Review acceptance completed. Ordinary resume and selection retain their existing
deadline checks.

Add `af/task-inspection@8` when recorded history includes the new transition. It preserves the
raw versioned history and supports ordinary Review recovery without inventing an Integration
phase. Prior inspection generations and their wire contracts remain unchanged. Fresh show and
explain are read-only; explain preserves the recorded Inconclusive exit status.

## Considered options

- Extending the deadline or resetting resources would authorize execution beyond the original Task.
- Replaying ordinary work after expiry could repeat paid calls or pure operations with side effects.
- Treating a captured Reviewer result as Task satisfaction would skip remaining acceptance work.
- A prefix-bound recording transition retains the missing fact under current authority and keeps
  execution permanently fenced by the original deadline.

## Consequences

The Store distinguishes permission to record existing evidence from permission to execute work.
Regression controls cover changed scope and authority, stale writers and Rounds, output arriving
after the failed report, and refusal to execute paid or pure work during recovery. Fresh CLI
inspection checks raw transition identity and unchanged database and event-log bytes. Full
checkpoint verification and specialist review remain separate evidence.
