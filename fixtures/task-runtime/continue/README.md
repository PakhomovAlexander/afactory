# continue

A reviewed Task continued from its candidate, for
[ADR-0203](../../../docs/adr/0203-continue-a-task-from-its-reviewed-candidate-and-give-judging-workers-the-base.md) and
`crates/af/tests/it/task_continue.rs`:

- `first.json` — a reviewed implementation on `fixture/implementation`. Its implementer writes
  `pagination.py`; the `fixture/bugs` reviewer reports one major finding while the tree has no
  `NOTES.md`, so the Task ends `changes_requested`.
- `af task run second --from first --execute` plans `second` from `first`'s captured Task file.
  Its source is `first`'s reviewed candidate, and its implementer receives that round's
  findings on the optional `findings` input: it asserts the candidate's `pagination.py`, writes
  the findings document it read to `seen-findings.json` and adds `NOTES.md`, so the review passes.
- `short.json` — a Task file with only `task_id`, `kind`, `goal` and `context`. `.af/af.toml`
  `[task.defaults]` supplies its limits and Pipeline, and `.af/rules.md` and the two context
  files are listed to the Worker by digest.

The `fixture/bugs` reviewer and the `fixture/evaluator` also assert what every Worker that judges a
change without writing it gets: the tree before the change, read-only, at `../base`, and
`../diff-subject.json` (`af/diff-subject@1`) listing each changed path with its content digest on
each side. An Attempt that finds either missing, wrong or writable fails, and the Task with it.

This repository is `bound-inputs` with the optional `findings` input on `fixture/implementation`
and `fixture/implementer`, a reviewer that asks for `NOTES.md` and the base assertions, so the
digests of the packages it changed are written as zeros; the test pins every package digest from
the copied tree and commits before running anything.
