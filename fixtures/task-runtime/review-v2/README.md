# Two-Round generic Task Review fixture

This token-free fixture uses `af.task-catalog/2` with `review.generation = 2`, source-scoped
`af/TaskReviewAssignment@1`, compact `af/TaskReviewSubject@2`, and
`review.kernel/ReviewerResult@2`. Two explicitly configured Review stages share one Task
and its six original Attempts. The first stage records a Finding and a required Demand;
the second supplies the exact prior-Finding disposition. The final domain conclusion is
`convergence_exhausted` (exit 3), while the completed Review goal is satisfied.

Copy the fixture to a disposable repository, commit its authority and run:

```sh
af review run --file review.json --state /an/absent/disposable/state --json
```

The sibling `review` fixture uses the same contracts in a single Round. A Diff Subject's
`change_scope.patch` declares its readable sandbox file path, exact content ID and byte count.
The native Worker reads that file under the existing bounds; it is absent from source outputs.
