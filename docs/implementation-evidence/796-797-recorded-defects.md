# Issues 796/797: bounded recorded-defect continuation

Implementer evidence only; this candidate is **incomplete**. The continuation consumes Snapshot
`sha256:5148b08a19dd691df02103fba699dcea073d34b3ce09f4caf68d6dc8105a3559`
and native findings artifact
`sha256:783eeb52597eb74fa0320ef47afecfbe2bfd4522a337e15eaa18fbad5f94961c`.
Historical [implementation](796-797.md), [follow-up](796-797-followup.md) and their failed/passed
execution records remain unchanged. Nothing here overrides the supplied failed complete Review.

## Findings and implementation

- **bugs-2**, native reviewer artifact
  `sha256:167088ef72702fd05395174b7e20b938275e0ac2c25896deb6529c7706a4270c`:
  implemented an explicit `--raw-markdown` presentation for recorded native text, requiring
  `--name`, `--observed-output` (a distinct recorded integer Unix-ms port) and `--stale-after-ms`.
  Producer text stays unwrapped. The same validated Markdown-block projection retains the native
  snapshot provenance, revision, observation/render times, status/freshness distinctions and
  terminal-control protection. JSON remains the default, with no content guessing. The authored
  fixture and native process test exercise one-second atomic watch, stable source observation
  time and clean interruption of a waiting Run without dispatch or mutation.
- Scheduler stop race
  `sha256:93eff87eb010ee85ebd5b7c9663de61653d5e28c5a25562b0f22563480089355`:
  implemented one immediate SQLite transaction spanning exclusive lease acquisition and row
  initialization. Stops serialize with startup and cannot be acknowledged then erased by that
  generation. A deterministic private test seam controls the former race window for first startup
  and restart with a previous row. An independent zero-timeout writer proves that the window is
  protected by the Store transaction; channels coordinate the public stop call. After acknowledgement,
  the driver's heartbeat-before-tick order admits no eligible work. Later explicit restart remains
  possible. No production control flag or ownership/lock bypass was introduced.
- **bugs-1**, the same native reviewer artifact: **UNRESOLVED**. No native bounded retention,
  compaction, protected lifecycle capacity or sustained regression crossing the 16 MiB bound has
  been implemented. Every occurrence/Run still remains in the World; a full journal can prevent
  settlement, cancellation and reconciliation. Limits, accounting and history have not been
  weakened, raised, discarded or moved into another ledger to conceal this defect.
- **evaluator**, artifact
  `sha256:1f52ea857e31b5cfe52de2f661dea010cf5797af1640efa1e4170a3e29f8c0fb`:
  **UNRESOLVED**. The supplied rejection and missing final-candidate acceptance remain binding.
  In particular, interval-scheduler and integrity-tests-docs acceptance cannot pass with bugs-1
  remaining. New independent correctness evidence and required reviewers/gate remain necessary.

The behavior is recorded in
[the new proposed ADR](../adr/2026-11-11-serialize-scheduler-start-and-select-recorded-markdown.md),
Scenario usage documentation, result fixture documentation and a change note. There are no
persisted shape changes or compatibility readers in these two repairs. The captured personal-account
pipeline, shipped product authority, local gate, storage limits and existing assertions are unchanged.
No publication, credentials, service installation, external messages or live monitor changes occurred.

## Focused execution

Darwin/arm64; the pinned Rust 1.88.0 toolchain selected explicitly on PATH. Cargo used
`--offline --locked`, checkout-local `target/`, `CARGO_HOME=$PWD/target/worker-cargo-home`
(registry-only links to the pre-existing cache, with no credentials/config copied) and
`TMPDIR=$PWD/target/worker-tmp`. Native process tests use `crate::common::af()` with private HOME/XDG
roots. These are local implementer tests, not sealed-Snapshot independent evidence.

| Command | Exit | Result |
| --- | ---: | --- |
| `cargo test --offline --locked -p af-scenario --lib scheduler -- --nocapture` | 0 | Initial focused startup/lease tests: 3 passed |
| `cargo test --offline --locked -p af-scenario --lib` | 0 | Final source: 63 passed, 0 failed, including schema/replay, unknown accounting, interval and controlled-race regressions |
| `cargo test --offline --locked -p af --test it native_authored_markdown_watch_uses_recorded_observation_and_never_drives_work -- --nocapture` | 0 | Native authored text, observed metadata and one-second atomic read-only watch; interrupt preserves waiting Run |
| `cargo test --offline --locked -p af --test it -- scenario::interval_scheduler_executes_real_one_second_occurrences_and_generic_results scenario::markdown_watch_interrupt_is_read_only_atomic_and_retains_active_run scenario::recorded_observation_results_are_partial_then_final_and_new_occurrences_change_revision scenario::scheduler_fatal_journal_failure_stops_and_keeps_run_and_diagnostic scenario::native_authored_markdown_watch_uses_recorded_observation_and_never_drives_work` | 0 | Final focused process suite: 5 passed, 0 failed; 6.78s excluding compilation |
| `cargo fmt --all -- --check` | 0 | Final Rust formatting |
| `cargo clippy --offline --locked -p af-scenario -p af --all-targets -- -D warnings` | 0 | Both changed crates and their test targets lint cleanly |

Failed setup/development attempts are retained here:

- The first scheduler-test command used the inherited Cargo location outside the writable sandbox.
  Cargo reported an operation-not-permitted cache write and waited on the public registry; the
  attempt was interrupted (exit 130). No result was counted as a test pass.
- The first offline-cache retry exited 101 because rustc could not create temporary directories
  under inherited TMPDIR. Selecting the private writable directory above resolved this setup issue.
- The first native authored-watch test exited 101 because its reused decision/Action step timeout
  exceeded the small static example's parent limit. The new test now reuses the complete coherent
  baseline fixture limits with those steps. No existing fixture, budget or assertion was weakened;
  the next individual run and the final five-case suite passed.

The unchanged complete local fast gate was **NOT RUN by this implementer**, as requested; the native
pipeline's exact-candidate gate remains required. Independent correctness build/private stage/test
`ImplementationTestEvidence` and final reviewer/evaluator conclusions are **NOT AVAILABLE in this
role**. No sustained-retention, new production performance, publication or full acceptance pass is
claimed.
