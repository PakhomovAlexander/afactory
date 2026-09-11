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
      | inputs: S0, S1, explicit empty Review history  |
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
          explicit Task confirmation
                  |
            new local worktree
```

`ticket.json` requests `"verification": "review"`. The Task therefore requires
`af/ReviewedImplementation@1` evidence under the captured Review policy. Its acceptance
requirement is determined before compilation. A Pipeline that supplies only the independent
evaluator result cannot satisfy that requested contract.

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

The fixture spends four Attempts: one implementation, one aggregate check and two reviewers.
It has no additional evaluator or child allowance. Its `review.json` selects the identical
Review package for standalone use; the integration test runs that package on the delivered
source with three Attempts. Standalone Review keeps its own business meaning: a complete
finding-bearing Review can satisfy the Review Task while still requesting changes.

Bounded repair, generated plan approval and Git catalog sync remain subsequent implementation
packages. This checkpoint proves composition and local binding without claiming those features.
