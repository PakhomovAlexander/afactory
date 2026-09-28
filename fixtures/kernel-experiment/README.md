# `kernel/experiment` for this repository

The experiment Pipeline and the `release_build` measure of package R2 of
[`docs/design/research-pipelines.md`](../../docs/design/research-pipelines.md)
([ADR-0124](../../docs/adr/0124-measure-and-compare-source-candidates-in-the-kernel.md)),
staged here because a Task Worker may not edit `.af/`. Installing them is a reviewed hand edit:

1. Append `code-policy-measures.toml` to `.af/code-policy.toml`. This changes the code policy's
   identity: re-pin any Worker package whose `[signature.evidence]` names the old policy digest.
2. Copy `task-packages/kernel/experiment`, `task-packages/kernel/experiment-trial` and
   `task-packages/kernel/experiment-evaluator` to `.af/task-packages/kernel/`.
3. Add the three packages to `.af/task-catalog.toml` with the versions and digests in
   `catalog.toml`, the paths under `.af/task-packages/kernel/`, and a `[providers]` entry for
   `kernel/experiment-evaluator` on a principal distinct from the implementer's.

`catalog.toml` and `contracts.json` are the shared catalog and contract fixtures of these
packages, so the staged copies are checked as they are:

```sh
af catalog test --source . --manifest fixtures/kernel-experiment/catalog.toml --json
```

`crates/af/tests/task_experiment.rs` performs the three steps on a copy of `.af/`, runs that
command, and plans a `kernel/experiment` Task with zero Attempts. The measured command is
`scripts/measure-release.sh`.
