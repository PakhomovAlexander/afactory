# ADR-2026-11-17-compose-scenario-terminal-evidence-with-kernel-failures: Kernel failures in Scenario receipts

**Status:** proposed (2026-11-17).

## Context

The issue 834 implementation source and native nightly releases must compose with merged
PR1064 (`afbccdddc1762c45d0bf7c02d85c0bf5e82105f1`). Native Tasks now distinguish
`execution_failed/kernel_execution` from ordinary gate failures and budget exhaustion.
Scenario terminal receipts embed those closed outcome and cause enums. Keeping their earlier
schema would let the writer emit evidence its own schema refuses.

The captured source is `554f76be35cb090022c9df54231fe6ba486d84ac`, from native Task
`mac-834-help-wrap-repair-20261117`. The supplied merged-main patch runs from
`42d5b0152aa270df39c2b989b9d5e8477d6a84df` to PR1064 and has SHA-256
`5585c28bffe4a780badfe1264ebac8f2aaeb6c5d71ea764cb146d104aaeca94a`.
Its inherited verification does not establish acceptance of this combined candidate.

## Considered options

- Treat kernel failures as gate failures: rejected; this hides the actual failure and can
  authorize a repair that the kernel did not admit.
- Reject all failed Tasks: rejected; ordinary gate and review failures retain their bounded
  native continuation path.
- Expand the earlier persisted shape in place: rejected by ADR-0145. Earlier Stores and
  plans must fail closed rather than acquire new interpretation.

## Decision

Retain PR1064's current Task inspection, report, schema and budget-precedence contracts.
The fallback kernel-failure cause applies to incomplete execution. A completed domain operation
with inconclusive evidence retains its existing terminal disposition; it is not a failed native
invocation. Explicit failed-node evidence and budget precedence remain authoritative.
Write `af/scenario-task-outcome@2` with the new terminal outcome and cause. Kernel execution
failure is neither accepted nor repairable, even with candidate, check and findings evidence.
Ordinary gate/review repairs still require the exact native evidence, known accounting and
remaining authority. Independent whole-goal verification remains required for publication.

Version the containing shapes together: Scenario/Bundle 10, Pin/Run/List 11, Store 17,
adapter request 8, Ant 11, Ant Pin/Tick 12, Ant State/List 13, Ant inspection 8,
workspace state 6 and GitHub journal 9. Shapes referring only to unchanged scalar or receipt
definitions keep their versions and point to the current schema. Writers, readers, shipped
schemas, generators, release declarations and positive/retired tests change together.
No earlier Store or plan is rewritten, migrated or accepted through an alias.

This composes the existing scoped publication, terminal-repair, driver and nightly decisions;
it supersedes no additional publication, Wait, repair, signing or merge authority clauses.
The issue 834 decisions retain their precise supersession of ADR-0274.

## Consequences

An isolated native regression creates an unchanged candidate, reaches the real empty-Diff
kernel refusal, retains its terminal receipt and accounting, and resumes without dispatching
a repair or publication. Existing native regressions retain ordinary multiple repairs,
independent verification, uncertain publication reconciliation, bounded transient waits and
restart coverage. These checks establish only the paths they execute. Physical VM reboot,
mid-Worker termination, paid Provider behavior, live GitHub publication and final independent
acceptance of this combined candidate remain separate evidence; no such success is inferred.
