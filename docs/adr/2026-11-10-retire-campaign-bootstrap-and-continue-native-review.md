# Retire Campaign bootstrap and continue standalone native Review

**Status:** accepted (2026-11-10); implements the owner's approved consolidation scope.

## Context

The [consolidation decision](2026-11-08-consolidate-current-contracts-and-task-review.md)
requires native Review capability closure before deleting Campaign execution. Its native Task
path already owns reduction, repair, Integration and accounting. Campaign bootstrap and Registry
readers still obscure that ownership, while standalone continuation needs canonical ReviewHistory
and an exact Base rather than the implementation continuation's findings port.

## Decision

Remove Campaign bootstrap, event branches, projections and Reviewer Registry/manifest readers.
Keep the shared canonical Ledger and Finding identity policy. Native Review alone initializes
its Ledger through `Ledger::for_task_subject`; typed Task publication retains CAS, fencing and
integrity checks. Generic Store append grants no canonical Review publication authority.

Standalone Review continuation plans a new Task from the completed Round's exact SourceTree,
original Base and canonical history. Reconstruct each original Round under its captured plan
and graph before reusing its claims. Preserve original policy identity, successor acceptance,
history-depth and Round bounds. Resume retains the original Task; a new continuation accounts
for its own Attempts without altering its predecessor's charges. No repair receipt is fabricated.

Changed closed shapes take new identities under ADR-0145: `review.kernel/RunEvent@3`,
`af/provider-models@4` and Project policy version 2. Old shapes fail closed. Unchanged Task
event, inspection, report, compiled graph and Pipeline shapes keep their current identities.
Immutable captured Provider catalog observations retain their recorded schema and validation;
they are evidence, and grant no current execution or configuration fallback.
Provider output exposes only current Worker and binding templates. Project policy retains Task
defaults, delivery, Survey and Brief authority without Campaign routing/default metadata.

The telemetry observer summarizes committed native Round outputs using their original plans;
it supplies no execution or acceptance authority. Incomplete Rounds never supply a partial Ledger.
Historical release dispatch, installation, rollback, signatures, byte pins and generic storage
inventory, locks, retention and pruning remain unchanged. This decision authorizes no publication
or real state pruning.

## Evidence and consequences

The [capability matrix](../design/current-contracts-and-task-review.md#native-review-capabilities)
names native tests and owners. The generated onboarding scaffold regression exercises actual CLI
continuation with both file and CLI Bases, exact-plan resume, prior claims, separate accounting
and bounded refusal. Schema parity tests cover current writers and retired-shape refusal.
Local commands and results belong in the Task's ImplementationReport. Independent native fast
gate, correctness and evaluation follow sealing; the cumulative full-profile gate remains the
later final-verification stage's mandatory patch-delivery condition.
