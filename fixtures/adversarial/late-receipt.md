# Case: late receipt from a fenced attempt

**Phase 3.** Discharges: *"A receipt from a fenced attempt is quarantined and charged but can
never feed downstream"* and *"Broker operations fail after attempt fencing even when the sandbox
process survives."*

## Why there is nothing to capture

The shell harness ingests a reviewer's findings file whenever the orchestrator hands it over. It
has no attempt identity, no epoch, and no notion of a result arriving after its attempt was
abandoned — so a late result is simply ingested as if it were current. The failure this case
describes cannot be reproduced against it, only against the kernel.

## Setup

1. Node `deep@1` starts attempt **A1** with epoch `e1`. Its sandbox process survives beyond the
   kernel's view (a stuck subprocess, a detached child, a hung network call).
2. The attempt times out. The kernel fences `e1`, seals the node, and starts attempt **A2**
   with epoch `e2`.
3. **A2** completes normally and emits a Finding Report `F2`.
4. *Then* A1's process finishes and delivers its own Finding Report `F1`, plus a broker call
   (an artifact publication and a privileged fetch), both stamped with epoch `e1`.

## Required behavior

- `F1` is **quarantined**: recorded as an immutable artifact with its attempt and epoch, marked
  fenced, and excluded from every downstream consumer — the FindingSet, convergence, gate
  decisions, patch linking, and the report's finding list.
- The cost of A1 is still **charged** against the node, binding, and run budgets. A fenced
  attempt is not a free retry.
- A1's broker calls **fail** at the broker, not at the sandbox: fencing revokes authority even
  though the process is alive and holds what it believes is a valid handle.
- The run's verdict is computed from `F2` alone and is identical to a run in which A1 never
  delivered anything.
- Replaying the event log with A1's delivery moved to any position — before A2 starts, between
  A2's start and completion, after the run converges — produces the same projections and the
  same verdict.

## What failure looks like

Silent. `F1` is a plausible finding from a real reviewer; ingesting it produces a run that
converges on a superset of findings, or reopens something A2 confirmed fixed. Nothing in the
output says a fenced attempt contributed. That is why the quarantine must be a recorded event
rather than a filter applied at read time.

## Status — implemented

`crates/review-attempt/tests/late_receipt.rs`. All three required behaviours hold:

- **Quarantined.** A1's result is recorded with its attempt and epoch, marked quarantined, and
  excluded from selected outputs — a downstream consumer sees A2's artifact and only A2's.
- **Charged.** A1's cost is committed to the run and node budgets whether or not anyone reads its
  answer. A fenced attempt is not a free retry, and a separate test shows a cap bounding a retry
  loop precisely because the wasted attempts count against it.
- **Replay-stable.** A1's delivery is moved to three positions — before the retry is dispatched,
  while it runs, and after it answered — and the selected outputs, the total spend, and the
  quarantine count are identical every time.

One behaviour fell out that the case did not ask for and should have: dispatching a retry fences
its predecessor without an explicit timeout. Otherwise a superseded attempt could win by
finishing first, which is the same failure arriving through the door marked "normal operation".

Not yet covered: the broker half — *"broker operations fail after attempt fencing even when the
sandbox process survives"*. There is no broker yet. When there is one, revocation belongs here,
and this file stays open until it is tested rather than being marked closed on the strength of
the parts that are.
