# ADR-2026-11-11-bracket-native-task-display-observations: Bracket native Task display observations

**Status:** proposed (2026-11-11).

## Context

The rc.3 contract consolidation replaced Campaign execution with native Task Review. The waiter
optimization must preserve that architecture while validating every byte used by a reused view.
Checking Task watermarks before display-artifact verification leaves a race: a writer can finish
during those reads and the waiter can return cached running status after its timeout expires.
Newly constructed views and final status documents have the same race: their dependent reads
must finish before the observation is checked, including indexed Gate history and its CAS reads.

## Decision

Keep the bounded artifact memo, streaming canonicalization and indexed Gate history described by
[the observation decision](2026-11-10-reuse-validated-artifacts-for-task-observation.md). Check the
Task and every replayed stream prefix before verifying the projection and display artifact closures,
and again after all those reads. A changed prefix reconstructs the projection and display through
ordinary validation. Replay tracing is scoped to the current Store and thread, includes nested
reads and retains the older prefix if the same stream advances during an operation.
Retain the oldest prefix across successive display operations too. Before either timeout or
settled return, construct the complete status document, track all its artifact and stream reads,
and validate that closure together with the projection and view. A changed prefix discards the
document, refreshes the projection and reconstructs the view and document before deciding the
exit. Unchanged data does not trigger a retry; steady running polls keep their existing byte
verification and 250 ms interval without repeatedly constructing Gate history.

Native Review integrity regressions exercise current Round, history, Integration and accounting
evidence. Retired handoff readers and DTOs are removed. Gate history indexes current
`TaskTransition@6` rows and validates each competing Task before using its measurements. Fixture
origins use the current typed writer. The current CLI adapter declares all six real waiter cases
and judges the native Ledger envelope with its independent schema and Finding enumeration rules.

## Considered options

- Check only before display reads: rejected because a committed completion can be missed.
- Trust notifications or artifact metadata: rejected because neither verifies current bytes.
- Restore Campaign handoff tests and readers: rejected because current native Review receipts
  and the common Task log own the evidence and accounting.

## Consequences

No persisted or public shape changes, no compatibility reader, no new publication authority and
no relaxed budgets or integrity checks. Deterministic tests commit a completion during display
verification and advance another observed stream while the Task itself stays unchanged.
CLI regressions finish a Task through ordinary execution while debug-only barriers hold initial
and refreshed view construction with an expired timeout, or hold an actual final Gate-history
artifact read. The same history-read regression covers an initially settled planned Task, so its
planned refusal also requires a checked observation. The final document must match fresh status.
Native Review tests remove or mutate observed evidence, require warm and cold refusal, and restore exact
accounting and selected outputs with the original bytes. Earlier GNU/shared-host CPU reductions
remain historical pre-reconciliation evidence; this change makes no rc.3 speedup claim.
