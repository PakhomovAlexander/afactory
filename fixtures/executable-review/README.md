# Executable review fixture

`program.c` builds independently with `cc program.c -o program`. Run `./program exact` for
success and `./program wrong` for exit 1.

The native package example lives in
[`task_review_execute_checks.rs`](../../crates/review-pipeline/tests/it/task_review_execute_checks.rs).
It captures ordinary reviewer, evaluator and pipeline packages with their typed contracts and
schemas, a required reviewer policy, and a local gate. An in-process adapter stands in for both
Workers; no Provider is contacted and no real model call runs. The reviewer actually compiles this
captured C source, copies its executable into `target/private/bin`, executes the installed bytes,
and returns `af/ImplementationTestEvidence@1` alongside its unchanged Reviewer Result. The evaluator
receives the exact envelope and Snapshot. `accept` assembles its ordinary verification receipt.

Run:

```sh
cargo test --locked -p review-pipeline --test it executable_review_evidence
```

Cases include successful execution, actual installed-program failure, unavailable testing, stale
Snapshot identity, missing output, and source immutability. Existing non-opting-in scratch/edit
cases in the same module remain active. This fixture demonstrates package contracts; production
consumers supply their own independent Worker bindings, models and project-specific commands.
