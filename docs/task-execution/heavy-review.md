# Heavy Review after bounded repair

A heavy implementation strategy can require a complete Review of its repaired Snapshot. The
generated or configured graph declares every round and its budget before dispatch.

```text
                      one Task / one allowance

S0 -- implement --> S1 -- call review --> closed discovery Round 1
                          checks + all reviewers          |
                                                   findings present
                                                         |
S1 ------------------- repair -------------------------> S2
                                                         |
                                               checks + fix verifier
                                                         |
                                                  review_continue
                                                         |
                                             exact S2 repair evidence
                                                         |
S0 + S2 + original history -------------------- call review
                                                   Round 2
                                               checks + all reviewers
                                                         |
                                            complete S2 Review receipt
                                                         |
                                      evaluate exact requirements on S2
                                                         |
                                    require Review AND goal acceptance
```

The first Round stays closed and finding-bearing. The continuation carries exact current-S2
per-Finding receipts into the second Round's projection. Positive independent decisions with
passing checks mark the corresponding original claims fixed. Missing or negative evidence leaves
them open. If a reviewer rediscovers a fixed claim, the canonical reducer reopens that same claim.
Other required Demands and scope failures also continue to block convergence.

The `review_continue` operator exposes `af/TaskReviewContinuation@1`. It consumes `source`,
`repair`, `checks` and the configured `verification` result. A missing runtime verification result
is recorded as inconclusive evidence; it cannot silently repair the Ledger. The next
`review_bind` accepts that artifact through its optional `continuation` input alongside the exact
original `history`. All other Review inputs and required reviewers remain present.

This continuation supplies no complete-Review coverage. The parent obtains that coverage from
the later Review call and its exact checks through `review_accept`. Projects can use this heavy
route with `allow_targeted_repairs = false`. A light route that accepts only targeted fixes keeps
its distinct `af/RepairAllowedImplementation@1` contract and one discovery Round.

The CLI integration fixture verifies two Rounds, eleven total Attempts and nine protected
verification Attempts. It covers positive, negative, unavailable, stale and rediscovered claims,
plus exact finished replay. The Store projection fixture also proves stale-view refusal before
mutation and reopening when a later Subject changes. See
[ADR-0059](../adr/0059-carry-task-fix-evidence-into-bounded-heavy-review.md).
