# ADR-0083: Run post-Round Integration within the original Task

Date: 2026-09-12
Status: Accepted (2026-09-23); superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): earlier
inspection, transition, run-report and handoff generations. Selection and completion are ordinary
changes of `TaskTransition@5`, a phase report is `af/TaskRunReport@2` with a `phase_id`, an
integrated handoff is `af/TaskReviewHandoff@2`, and Integration phases are an optional section of
`af/task-inspection@11`, the only inspection.

## Context

Automatic Integration begins after Review has selected sealed Proposals and closed a passing
Round. Its check sequence must use the common Task runtime while preserving that immutable
Review conclusion. A second scheduler or a new Task budget would split ownership of execution,
recovery and paid usage. Making the sequence an ordinary dependency of the Round would require
it to run before the evidence that authorizes it exists.

## Decision

Capture the optional sequence in the installed `LegacyReviewTaskPolicy@4` and compiled Task.
It is dormant until the Store selects an exact `TaskReviewIntegrationPhase@1` after the canonical
Round conclusion. Its fixed node and original allowance are installed before execution; the
phase receipt carries no new budget, approval or arbitrary executable graph. Light Review and
the final permitted heavy Round install no unusable sequence allowance.

Selection records one of Empty, Conflict or Prepared. Empty and Conflict finish without another
Attempt. Prepared binds the exact Integration Plan and unpromoted derived Snapshot. One common
Task Attempt runs the complete captured check sequence, in declaration order, within one shared
writable sandbox. It uses the original Gate policy, check timeout and absolute Task deadline.
The existing Runtime owns preparation, reservation, dispatch, heartbeat, output selection and
publication. Reopen reuses its durable output; it does not repeat successful checks.
This generation allows at most 63 checks so every raw CheckResult and one sequence summary fit
the existing 64-artifact settlement bound; a larger sequence refuses before execution.

`TaskRunReport@2` records the activated singleton operation without changing the Round's original
`TaskRunReport@1` or canonical `RunReport@6`. Protected `TaskTransition@3` events record selection
and completion. The Store transaction verifies the Task and Review prefixes, writer, original
resources, selected Proposal composition, derived Snapshot, checks and exact attestation lineage
before committing canonical Integration and Task completion together. Ordinary event append
cannot manufacture Task-backed Integration authority.

If successful checks are followed by late usage that breaches the original budget, promotion
must refuse. Preserve the completed check output and earlier report, then record a newer factual
resource-failure report at the current Task prefix and finish the phase without promotion. This
reports refusal to promote; it does not say the completed checks failed. Writer loss, changed
authority or unavailable evidence are separate failures and cannot use this recovery shortcut.

A committed Integration requires a subsequent complete Review of the derived head. Its
`TaskReviewHandoff@2` links the exact passing report, phase and IntegrationCommitted event. The
protected continuation retains the same Task, original limits and cumulative usage; it leaves
the successor plan unadmitted. Existing generated-plan approval rules continue to apply.
The original handoff generation remains reserved for its existing closed-Round and input-epoch
transitions. Canonical dispositions retain their established semantics: a `not_reproduced`
disposition alone is not a FixVerification receipt and does not declare a Finding fixed.

For the installed Review frontend, derive a lifetime resource envelope once from the captured
Round limits, bounded retries, fan-out, checks and Provider probes. Later Rounds and input epochs
reuse it. Existing Round and Worker token caps remain additional scopes within the same ledger.
An explicit trusted fallback is needed for previously uncapped work; it never overrides captured
caps. The CLI's capture and resume policy is part of its separate execution cutover.

Read-only `af/task-inspection@7` exposes historical phase records, exact report generations and
handoffs. It preserves original CAS payloads and earlier inspection generations. Historical
plans remain inspectable without regaining admission or dispatch authority.

## Considered options

- A second scheduler would duplicate supervision, retry and accounting decisions.
- An ordinary Round node cannot depend on a conclusion produced after that Round completes.
- A newly compiled arbitrary phase graph could introduce uncaptured work or resources.
- A dormant installed operation with protected activation uses the existing execution contracts.

## Consequences

Review retains its canonical domain model while Task remains the resource and execution owner.
The Store retains phase evidence across later Rounds, and the public inspection must distinguish
historical evidence from active execution. Complete failing checks and empty or conflicting
selection cannot promote derived output; unresolved authority or resources cannot establish
verified acceptance. Integration never creates or updates a developer branch or PR.

Implementation verification is in progress. This decision does not itself claim completion of
the legacy CLI cutover, requested specialist reviews, live pilot or release.

Frozen implementation tree `a53e534fe1c7d3082a259606639a35240007aa9d` passes the full local gate
at `d2df0ec`: 1,044 tests, zero failures, 15 ignored, formatting, Clippy, documentation tests
and byte-identical fixtures. Product commit `6fe82f0` has that exact tree. The legacy CLI and
specialist review remain separate unfinished work.
