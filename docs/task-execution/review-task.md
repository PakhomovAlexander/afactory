# Review Tasks

The versioned Task-file entry point runs standalone Review through the common Task runtime:

```text
af review plan --file review.json --state /tmp/af-review-state --json
af task explain review-cli --state /tmp/af-review-state --json
af task run review-cli --execute --state /tmp/af-review-state --json
```

`af review run --file review.json` captures, plans and executes in one command. The file must
declare `kind = "review"`. Its Pipeline and Worker packages come from committed
`.af/task-catalog.toml`. The catalog's `review` table declares required reviewer names, their
Demand requirements, the severity gate and bounded convergence policy. Its optional
`generation` key may only be `2`, the one Review generation; omitting it means the same
`af.review-task-policy/2`. The existing captured
code-check policy supplies the required checks. Command Workers and native Model Workers use
the same runtime; [Model bindings](model-bindings.md) require account identity and charged
capability admission inside the Task.

The executable fixture is `fixtures/task-runtime/review`. Copy it into an empty directory,
initialize and commit a Git repository there, and run the commands above. It has two command
reviewers and one deterministic check. One reviewer reports an open major Finding, so the
final `run` exits 3. No credentials or model calls are needed. `fixtures/task-runtime/review-v2`
runs two Rounds, so its second Round disposes of the first Round's Finding.

```text
                    one Task / Store / budget
                    =========================
 captured source ------+--------------------------+
                       |                          |
 empty/prior history -> review-bind                check
                       |                          |
                       +---- typed Subject -------+
                       |                          |
                  correctness                    bugs
                       |                          |
                       +----------+---------------+
                                  |
                           review-reduce
                         /                \
                complete gather       missing required input
                      |                        |
              FindingSet + DemandSet    selected results remain
              closed Round + history   visible; no partial Ledger
                      |                        |
             Pass / ChangesRequested          Incomplete
```

`review-bind` derives a whole-tree or diff Subject from declared immutable source ports. Diff
binding compares the captured manifests and preserves the canonical patch, changed paths and
rename policy. The `af/TaskReviewSubject@2` Subject carries that scope and declares the patch
as a read-only file with its exact content ID and size. Binding an empty Diff is refused. The
prior history input is explicit; only a root port declaring the empty Review history default
can synthesize the initial value. `review-bind` also emits one `af/TaskReviewAssignment@1` per
configured reviewer, listing only that reviewer's own current prior Findings. The reviewer's
`review.kernel/ReviewerResult@2` must dispose of each assigned Finding exactly once.

`review-reduce` is one atomic gather and canonical reduction barrier. Every configured reviewer
must have a binding in the definition. Reviewer bindings use the reserved verification role,
the same source, Subject, checks and history as the reducer, and their own assignment from
`review-bind`. Optional reducer ports allow
failed or suppressed execution to produce an incomplete receipt; they do not permit omitting
a required reviewer from the definition. No authoritative partial Finding or Demand Set is
published. A failed reducer itself leaves the public obligation missing.

The domain reuses the canonical reducer and Finding projection. Immutable prior Rounds rebuild
that same Ledger without a second database or Attempt allowance. Review history preserves the
original closed Round and exact canonical views; a failed gather leaves the previous history
intact. Final admission recomputes the receipt against its recorded Task, plan, policy and
selected inputs.

| Review output | Generic Review Task acceptance | CLI exit |
|---|---|---|
| Complete, clean | Satisfied | 0 |
| Complete, open blocking Finding or required Demand | Satisfied | 3 |
| Missing reviewer or unavailable required check | Inconclusive | 4 |

Completing a Review Task means producing its required review evidence. The separate domain
conclusion still governs whether the reviewed change passes. `af task run` retains these
Review exit codes on replay, and inspection includes the typed Round receipts.

See [Campaign Review](campaign-review.md) for the shared runtime boundary and
[bounded repair](bounded-repair.md) for current-Snapshot repair operators.
