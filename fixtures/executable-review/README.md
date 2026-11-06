# Executable review fixture

`program.c` builds independently with `cc program.c -o program`. Run `./program exact` for
success and `./program wrong` for exit 1.

The native package example lives in
[`task_review_execute_checks.rs`](../../crates/review-pipeline/tests/it/task_review_execute_checks.rs).
It captures ordinary reviewer, evaluator and pipeline packages with their typed contracts and
schemas, a required reviewer policy, and a local gate. Executable cases create native `implement`
Tasks with both requirements verification and reviewed-implementation acceptance. A child Review
pipeline exports two distinct evidence receipts on explicit ports to the parent evaluator. An in-process adapter stands in for both
Workers; no Provider is contacted and no real model call runs. The reviewer actually compiles this
captured C source, copies its executable into `target/private/bin`, executes the installed bytes,
and returns `af/ImplementationTestEvidence@1` alongside its unchanged Reviewer Result. The evaluator
receives both exact envelopes and their Snapshot. `accept` assembles its ordinary verification
receipt, which covers the implementation Task's acceptance obligation. Terminal validation runs
through `CodeTaskDomain`, including direct challenges after successful admission.

Run:

```sh
cargo test --locked -p review-pipeline --test it executable_review_evidence
```

Cases include successful execution, actual installed-program failure, unavailable testing, stale
Snapshot identity, missing output, and source immutability. Terminal challenges leave the first
input valid and make the second failed, unavailable, stale, wrongly typed, absent, multiple or
unretained; missing and foreign context are also rejected. Existing non-opting-in scratch/edit
cases in the same module remain active. This fixture demonstrates package contracts; production
consumers supply their own independent Worker bindings, models and project-specific commands.
