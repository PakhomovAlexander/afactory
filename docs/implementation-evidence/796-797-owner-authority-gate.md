# Predecessor caller-authority gate failure

Historical predecessor evidence, preserved including failed results. Fresh follow-up results are
recorded in [the bounded correction record](796-797-followup.md).

## What I saw

The unchanged fast gate's functional class rejects the captured, caller-provided
`.af/task-packages/owner-impl` authority. The implementation requirements explicitly require
leaving that directory and its catalog entries intact. No audit assertions or authority were changed.

## Evidence

On the pinned Rust 1.88.0 toolchain, with a private writable HOME/XDG environment, the final
functional class ran 1,839 Rust tests: **1,837 passed, 2 failed**. Both failures failed again in
the gate's automatic isolation reruns:

- `catalog_digest::every_package_declaring_diff_subject_tells_its_worker_to_read_the_input`:
  `.af/task-packages/owner-impl/evaluator` declares `diff_subject`, but its instructions do not
  contain the audit's required literal phrase `` `diff_subject` input ``.
- `catalog_digest::every_shipped_model_worker_uses_supported_61_models_and_independent_judges`:
  `owner-impl/bugs` has the caller's `codex-personal` binding; the audit expects `codex-main`.

The functional runner exits 1; its Rust nextest subprocess exits 100. The earlier full
`bash scripts/verify.sh --profile fast` invocation exits 2 through Make. The feature's previously
missing CLI effect metadata was repaired; all eight `command_catalog` tests pass afterward.

## Repro

Keep the captured owner authority and existing assertions unchanged. Use a private writable
HOME and XDG directories; put the pinned Rust toolchain first on PATH. From this captured tree:

```sh
TEST_THREADS=4 AF_GATE_TARGET_DIR="$PWD/target" bash scripts/verify.sh --profile fast
cargo test --locked -p af --test it catalog_digest::every_package_declaring_diff_subject_tells_its_worker_to_read_the_input
cargo test --locked -p af --test it catalog_digest::every_shipped_model_worker_uses_supported_61_models_and_independent_judges
```

No external issue was filed because this Task explicitly prohibits external messages and credentials.
