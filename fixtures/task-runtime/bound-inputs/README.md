# bound-inputs

Bound Tasks in one Store, for [ADR-0117](../../../docs/adr/0117-bind-task-inputs-to-recorded-task-outputs.md)
and `crates/af/tests/task_input_bindings.rs`:

- `base.json` — an ordinary reviewed implementation on `fixture/implementation`. Its Pipeline
  exposes the embedded Review's `history` beside `snapshot`, `verification` and `evaluation`,
  so a successor has a ledger to bind to.
- `successor-history.json` — binds `history` to `base`'s `history` output and runs
  `fixture/plain`, which hands that ledger to its implementer as context. It plans, runs and
  delivers: a bound `history` carries the referenced artifact verbatim, suppresses the
  `empty_review_history` root default, and does not touch delivery.
- `successor-source.json` — binds `source` to `base`'s `snapshot` output. That Snapshot is
  derived, so it is re-rooted over the identical Manifest with an `af.task-source-origin/2`
  origin that carries no `source_revision`. It plans and runs, and `af task deliver` refuses:
  the tree has no commit for a target repository's `HEAD` to equal.
- `successor-issue.json` — binds the same `history` and captures a local `issue.json` the test
  writes and commits. A source refresh over a changed issue must keep the binding record, which
  is provenance no port carries.

The test also writes one Task file of its own, `rebound.json`, because its `inputs` table names an
exact artifact ID that only exists once `successor-source.json` has run: the re-rooted
`af/SourceTree@1`. Bound again, that parentless generation-2 Snapshot is carried verbatim, and its
delivery refusal names the artifact rather than the Task the first re-rooting recorded.

The successors run `fixture/plain` rather than the reviewed Pipeline on purpose. A Review Round
is restored by recomputing it, and that recomputation requires every reviewer result to retain
the *current* Task's `af/Requirements@1` — which a predecessor's results cannot, because the
requirements artifact is derived from the Task file. A bound `history` therefore crosses into a
Pipeline that reads the ledger rather than one that continues the Round; see
`docs/task-execution/task-inputs.md`.

This repository is `embedded-review` with three changes — the `history` output on
`fixture/implementation`, the new `fixture/plain` Pipeline, and an optional `history` input on
`fixture/implementer` — so the digests of the packages it changed are written here as zeros.
The test pins every package digest from the copied tree and commits before running anything,
exactly as `crates/af/tests/task_acceptance.rs` does for the fixtures it edits.
