# Task validation costs and integrity boundaries

The captured Review compiler memo reuses one exact Task/Plan structure. Every use still verifies
all captured read dependencies. Historical Task evidence remains checked across Store calls;
no memo grants execution, approval, lease or resource authority. See
[ADR-0101](../adr/0101-reuse-review-structure-with-fresh-task-boundaries.md).

## Local measurements

These measurements used Rust 1.88.0 on macOS arm64, the locked offline dependency set,
`CARGO_BUILD_JOBS=2`, `CARGO_INCREMENTAL=0`, and dev/test debug information disabled. The
existing Store and serde_json development optimizations remained enabled. They compare full
read-only recompilation with memo validation of the same captured plan, 16 serial calls per arm.
All authority replacement/removal checks passed before measurement.

| Captured authority | Objects | Full p50 / p95, µs | Memo p50 / p95, µs |
| --- | ---: | ---: | ---: |
| Basic Review | 17 | 1701 / 2178 | 611 / 637 |
| Heavy Review V3 | 19 | 2989 / 3405 | 880 / 905 |
| Heavy Review V4 with Integration | 20 | 3190 / 3482 | 944 / 983 |
| Brokered Provider probe | 21 | 2411 / 2880 | 785 / 794 |

A captured two-Round handoff fixture retained 26 references / 24,543 bytes, or 27 references /
26,211 bytes with generated approval. Each cold or warm projection loaded the canonical Campaign
once. Across 16 serial warm calls, p50/p95 was 2296/2395 µs and 2283/2367 µs respectively.
The reference-byte count describes the closure streamed for integrity, not all typed decoder I/O.

These samples establish local method latency and exact operation counts. They do not establish
whole-Round speedup, performance with large native transcripts, eight-slice scaling, or Store mutex
wait under parallel Workers. An earlier parallel test-suite sample was noisy and included a slower
memo sample; it is retained with the command evidence and is not used to claim stable improvement.

## Repeated projection work

| Public path | Previous projections | Current projections | Reason |
| --- | ---: | ---: | --- |
| Record invocation | 2 | 1 | No domain callback before append |
| First reservation | 2 | 1 | No retry callback on the first Attempt |
| Start Attempt | 2 | 1 | Reuse the checked capability state |
| Bind context | 3 | 2 | Keep the fresh check after context admission |
| Failed settlement | 2 | 1 | No output-admission callback |
| Successful settlement | 2 | 2 | Keep the fresh check after output admission |
| Output publication | 2 | 2 | Keep the fresh check after output admission |
| Owned child publication | 3 | 2 | Reuse the post-callback check |
| Owned completion | 4 | 3 | Keep checks after both domain callbacks |

A reservation that invokes retry policy retains its fresh post-callback projection. The fixture
that prepares root inputs performs two invocation records plus an output publication: its combined
count changes from six to four. No cross-operation authority check is removed. Thus mandatory
historical integrity work can still grow with Task history; this change removes redundant work,
not that contract.

## Reproduction and limits

The normal `review-store` unit suite asserts exact per-path counts, cold/warm handoff replay,
revocation replay, callback corruption, and large raw/typed usage evidence. The captured Review
plan tests mutate engine, Campaign, policy, pipeline, lock, package files, dependencies, invocation,
probe, Integration and graph identities. The public runtime tests preserve retry and billing rules.

For the serial measurement fixtures:

```sh
cargo +1.88.0 test --offline --locked -p review-pipeline --test task_legacy_review plan:: -- --nocapture --test-threads=1
cargo +1.88.0 test --offline --locked -p review-store --lib store::task::tests::review_handoff::review_handoff_retains_original_budget_late_charge_and_exact_reopen_without_autoapproval -- --exact --nocapture --test-threads=1
```

The complete focused pass comprised 137 Store tests (one existing ignored test), 51 captured Review
tests, 23 runtime tests and scoped all-target Clippy. No model, Provider, authentication, CI or paid
review call was made. Two failed development checks are retained: a test import mistake, then an
invalid synthetic Attempt identifier and incorrect setup-helper count assertion. Production
acceptance criteria and authority checks were not relaxed to obtain the passing run.
