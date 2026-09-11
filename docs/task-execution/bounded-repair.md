# Bounded repair after embedded Review

The `bounded-repair` fixture implements a pagination ticket, receives an original S1 Finding
about negative offsets, repairs the code into S2, checks S2, and independently verifies that
Finding. It uses the same Review package boundary as ordinary reviewed implementation.

```text
Task: implement this Jira ticket                  one parent allowance: 7 Attempts
  |
  +--> implement S0 --> seal S1 --> call Review(S0, S1, empty history)
                                      |
                                      +--> checks --> reviewers --> canonical Round 1
                                                                   |
                             +-------------------------------------+
                             |
                         review_accept
                             |
              +--------------+--------------------------+
              | passed       | failed                   | incomplete
              v              v                          v
             S1       call bounded repair              incomplete S1
                      |  repair original claims
                      |  seal S2
                      |  attest S1-to-S2 paths, rebind original claims to S0-to-S2
                      |  check S2
                      |  independent fix verifier
                      v
                 targeted acceptance
                             |
                             v
                  typed Select of final Snapshot and acceptance
                             |
                  explicit delivery to a new local worktree
```

The repair call has a three-Attempt ceiling: repair, checks, and fix verification. The parent
protects five verification Attempts before implementation starts: three for initial Review
and two for repair. Conditional paths retain their bounds and selected values across resume.
A call that cannot hold its protected verification plus unconditional paid work is refused.
Neither a child call nor a retry creates another budget.

## Two explicit guarantees

`af/ReviewedImplementation@1` requires complete current-Snapshot Review. Targeted repair cannot
produce this acceptance type. A Task can explicitly request `verification =
"review_or_targeted_fixes"`, or use an installed `repair_allowed_implementation` Task-kind
profile, to require `af/RepairAllowedImplementation@1` instead. The captured project Review
policy must also set `allow_targeted_repairs = true`. This is independent of Worker settings.

The latter receipt records `scope = complete_review` when S1 passes the original Review and
`scope = targeted_fixes` when repair succeeds. The public type and exact verifier policy prevent
a targeted branch from satisfying a Task that requires complete S2 Review. A heavy strategy
must explicitly admit another discovery Round to obtain that stronger guarantee.

## Evidence and history

Review exposes a typed `af/TaskReviewClaims@1` repair input containing the original claim text,
remedy, view IDs and closed Round ID. Repair Workers receive these declared inputs without a
parent transcript or ambient Ledger access.

`attest_fixes` checks that S2 is the direct sealed descendant of S1 and computes the S1-to-S2
changed paths. It rebinds the original Ledger to the S0-to-S2 Subject without closing another
Round. `af/TaskRepairContext@1` retains original view IDs, current view IDs, per-Finding
`ChangeAttestation@1` artifacts and the exact `VerificationContinuation@1`.

A reserved `fix_verify` Worker receives only current source, repair context and passing current
checks. Its typed reply must account for every original Finding with the exact current view,
Subject and attestation. `repair_accept` emits one `af/TaskFixReceipt@1` per Finding and a
`RepairAssessment@1` with the distinct `targeted_fixes` scope. Missing output produces explicit
inconclusive receipts without impersonating a verifier Attempt. Negative receipts, failed
checks, unresolved required Demands and scope-authority failures cannot produce acceptance.

The original closed Round remains immutable, including its Finding-bearing conclusion. Task
inspection displays `review_rounds` and `repair_assessments` separately. Delivery follows the
bounded captured Snapshot ancestry back to exact S0; it never recaptures live HEAD.

## Credential-free example

Copy `fixtures/task-runtime/bounded-repair` into a new Git repository, commit the fixture,
then run these commands with a build supporting the Task increment:

```sh
af task plan --file ticket.json --json
af task run repair-cli --json
af task deliver repair-cli --confirm repair-cli \
  --branch pagination-fixed --worktree ../pagination-fixed
```

The fixture verifies one original negative-offset case and the existing pagination check. Its
seven command Attempts demonstrate mechanics and authority; they are not a paid-model pilot.
