# ADR-0059: Carry Task fix evidence into bounded heavy Review

**Status:** accepted

## Context

ADR-0054 distinguishes targeted repair verification from complete Review of S2. A heavy strategy
must preserve original claim identity while applying independently verified fixes before its next
discovery Round. Legacy FindingResolution admission assumes a different Change Set authority;
manufacturing those artifacts would misrepresent the Task's sealed S1-to-S2 repair evidence.

## Decision

Add a pure `review_continue` operator. It consumes the exact current source, repair context,
current checks and reserved independent fix-verifier output. It recomputes per-Finding receipts
and publishes `af/TaskReviewContinuation@1`, retaining the original closed Round, prior history,
assessment and actual invocation. Its signature grants no complete-Review acceptance coverage.

`review_bind` may consume this typed continuation. Its Subject context records the exact
continuation artifact, and validation rechecks the producing operator, plan, current Subject,
Snapshot, original history and next bounded Round. Neither a changed input nor a later arbitrary
Round can reuse the receipt. The original ReviewHistory and original closed Round remain unchanged.

Before canonical reduction of that next Round, restore the original Ledger, bind it to S2 at the
original Round number, and project the current receipts. Positive independent claims can become
fixed only after current checks pass. Negative, missing and stale evidence cannot fix a claim.
Then advance the projection to the next admitted discovery Round and use the existing canonical
reducer. A rediscovered fixed claim reopens with its original identity. Required Demands and
scope authority failures retain their existing convergence effect.

This is a Task-only projection API, not a Campaign event or legacy Resolution. Its caller must
recompute the assessment and validate Worker authority first. The projection additionally checks
receipt bytes and every current Finding view before applying any changes. Fixed status is bound
to that exact Subject and reopens if a later Task Subject changes. No historical Store behavior
or persisted legacy artifact is reinterpreted.

The heavy route is allowed by the captured discovery-Round bound. It does not require enabling
targeted acceptance. A final implementation acceptance still requires the complete Review and
checks of S2, with all required reviewers present. Each check and Worker uses the parent Task's
original allowance and protected verification capacity.

## Consequences

The real CLI fixture performs ten Attempts: implement, check and two S1 reviewers, repair, check,
fix verification, then check and two S2 reviewers. Eight verification Attempts are protected.
The success case preserves the S1 finding-bearing Round, fixes the same claim on S2, completes
Round two and records complete-Review acceptance. Negative, missing, stale and rediscovered claims
prevent acceptance. Finished replay preserves artifacts and spend without new Attempts.

The shared heavy starter and legacy Review entry-point migration consume this capability; the
new bridge alone does not complete either product surface or the release checklist.
