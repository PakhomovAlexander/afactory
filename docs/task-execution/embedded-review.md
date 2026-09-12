# Implement a ticket with embedded Review

The command-only project in `fixtures/task-runtime/embedded-review` uses one Task, one budget
and the same `fixture/review` package for standalone and embedded Review. Copy it into an empty
repository and commit the files before running the unreleased Task-file CLI.

```text
ticket.json: "Implement this Jira ticket: offset/limit pagination"
  verification = review
                  |
                  v
              implement(S0)
                  |
                seal S1
                  |
      +-----------v-----------------------------------+
      | call fixture/review                           |
      | inputs: S0, S1, requirements, empty history     |
      |                                               |
      | bind Subject(S0..S1) --+                       |
      |                       +--> two reviewers      |
      | check S1 --passed-----+          |             |
      |                          canonical reduction  |
      | outputs: Review, history, exact check receipts |
      +-----------+-----------------------------------+
                  |
             review-accept
                  |
       ReviewedImplementation + S1
                  |
       evaluate(S1, requirements, current checks)
                  |
       require Review AND goal acceptance
                  |
          explicit Task confirmation
                  |
            new local worktree
```

`ticket.json` requests `"verification": "review"`. The Task therefore requires
`af/ReviewedImplementation@1` evidence under the captured Review policy and a separate
`af/VerificationResult@1` against the exact Task Requirements under the code policy. The public
`verification` and `evaluation` outputs cover `verified` and `goal` respectively. Both obligations
are determined before compilation; both must pass.

```sh
af task plan --file ticket.json --state /tmp/reviewed-pagination --json
af task run pagination-cli --state /tmp/reviewed-pagination --json
af task deliver pagination-cli --state /tmp/reviewed-pagination \
  --branch task/reviewed-pagination --worktree /tmp/reviewed-pagination-result \
  --confirm pagination-cli --json
```

The parent binds the child to captured S0 and sealed S1. The child owns checks; the parent
consumes their public receipts. The compiler requires the child's public `reviewed` coverage
and matching Snapshot affinity. Acceptance recomputes the canonical Review and verifies its
exact plan, source, required checks and retained evidence. Findings, missing reviewers and
unavailable checks cannot become a verified implementation. Failed checks skip reviewers and
produce unsatisfied implementation acceptance.

The fixture spends five Attempts: one implementation, one aggregate check, two reviewers and
one independent evaluator. It reuses current checks and shares one Task allowance. Its `review.json` selects the identical
Review package for standalone use; the integration test runs that package on the delivered
source with three Attempts. Standalone Review keeps its own business meaning: a complete
finding-bearing Review can satisfy the Review Task while still requesting changes.

[Bounded repair](bounded-repair.md) selects S1 or S2 before the final goal evaluation. Failed or
missing evaluation prevents delivery even when Review passes. [Generated plans](generated-plans.md)
require signed approval; [Git catalog sync](shared-catalogs.md) shares the composition and Workers.
