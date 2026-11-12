# Issues 796/797: exact within-Run accounting continuation

Implementer evidence dated 2026-11-12 for captured source
`sha256:9bfbbb52e2327c7e5e4379f167bfd297b740132fe5d6f386b0ccbff7deda6081`.
The rejected candidate is
`sha256:4992ba1788b777dc4ab6aecfacc066d1316d31e906a541e43c9bb73b495f70be`;
required bugs result
`sha256:42d512e233d77d45e07593d841d286b008ef424cd6dc9e86a265df4419a1500e`
identifies the within-Run overflow. The supplied finding identity is
`sha256:0e66129a4e691c8b6b9e827f28d4ad918a9d6d9dcc245651370b1f7015c5503b`.
Its failed complete Review and evaluator verdict remain historical failures, regardless of
predecessor gate or executable passes.

## Repair and coverage

`Run::tokens` and `Run::accounted_tokens` now compute checked u128 sums. Run validation,
settlement and native reconciliation compare those exact totals against widened limits.
Prospective root and containing-frame reservations also use u128. Rolling admission converts a
Run total to a conservative u64 bound only at the comparison; the inherited across-Run and
prospective-reservation saturation remains. Retention's existing cumulative charge fold and its
rolling bucket writer both retain exact u128 totals. Individual charges and persisted shapes are
unchanged. Inspection emits the same native Run/Task charge records and decimal summaries;
there is no new numeric JSON field or schema/reader fallback.

The [dated clarification](../adr/2026-11-12-keep-within-run-accounting-wide.md),
[change note](../../changelog.d/ant-within-run-accounting.md) and
[Scenario guide](../scenarios.md#native-retention-and-capacity) describe the repair.

`retention::tests::within_run_reconciliation_overflow_replays_skips_and_retires_exactly`
uses an isolated native SQLite Store, controlled one-second ticks and the inert native Task Host.
No unvalidated Run is injected. Runtime dispatch settles the first Task for 17 tokens and loses
the second response, durably retaining its 40-token reservation. Reopening the Store preserves
both charges, Run execution identity, intents and dispatch count. A later producer is admitted
while those reservations still fit the ordinary budget.

Native reconciliation then recovers u64 maximum for the second Task. Its actual Store update
persists the exact aggregate **18,446,744,073,709,551,632**, releases only the resolved reservation,
and leaves a failed terminal Run that validates and replays. Preparing it dispatches nothing.
The actual Ant runtime tick records `skipped_budget`, persists cursor 22 and its tick timestamp,
and repeating that tick starts nothing. Ageing the body out preserves the latest producer and
moves the exact aggregate into both cumulative accounting and a live settled bucket. Reopening
before and after compaction checks those values. Admission remains suppressed just before normal
rolling expiry, then resumes exactly at the ordinary boundary; cumulative charges survive.
Execution and intent identities are checked before retirement; its provenance hash changes and
replays after the full body becomes unavailable.

The previous
`rolling_admission_saturates_retained_and_retired_overruns_and_replays_exactly` test remains
unchanged and passes both retired-plus-live and retained-only branches. The library suite also
covers unknown usage across expiry, conservative bucket merging, complete-loop reservations,
retention capacity, cancellation/recovery and one-second scheduling.

## Execution evidence

Darwin/arm64 with pinned Rust 1.88.0, offline locked Cargo and checkout-local `target/`.
Cargo commands use `PATH=/Users/apakhomov/.rustup/toolchains/1.88.0-aarch64-apple-darwin/bin:$PATH`,
`CARGO_HOME=$PWD/target/worker-cargo-home` and `TMPDIR=$PWD/target/worker-tmp`.
The private Cargo home links only the existing registry cache; no credentials or machine
configuration were read or copied.

| Command | Exit | Result |
| --- | ---: | --- |
| `cargo test --offline --locked -p af-scenario --lib within_run_reconciliation_overflow -- --nocapture` before repair | 101 | Actual native reconciliation update failed with `Scenario accounting overflow` |
| Same focused command after repair | 0 | New native lifecycle/replay/retirement regression passed |
| `cargo test --offline --locked -p af-scenario --lib -- --nocapture` | 0 | 74 passed, 0 failed; 122.36 seconds, including both overflow regressions and contract/schema roundtrips |
| `cargo clippy --offline --locked -p af-scenario --all-targets -- -D warnings` | 0 | No warnings |
| `cargo fmt --all -- --check` | 0 | Rust formatting passed |
| `cargo test --offline --locked -p af --test it cancellation_preserves_receipts_and_never_dispatches_later_effects -- --nocapture` | 0 | Exact successor CLI compiled; 1 passed, 1,039 filtered out; 2.37 seconds |
| `python3 -I scripts/adr.py list` | 0 | Dated decision discovered without index edits |
| `python3 -I scripts/changelog-notes.py --check` | 0 | Change-note format accepted |
| `python3 -I scripts/test-changelog-notes.py` | 0 | 13 passed |

During test authoring, the first run failed because its accepted receipt lacked a required output;
that fixture was corrected before reproducing the arithmetic failure. The first repaired run
also exposed an incorrect final count assertion: normal admission at expiry retires the later
zero-charge producer too. The final test asserts both terminal identities are counted while the
exact cumulative charge stays unchanged. These were test-authoring failures, not passing evidence.

The actual Markdownlint CLI is invoked with Node at
`/Users/apakhomov/.npm/_npx/38d773fe1452947e/node_modules/markdownlint-cli2/markdownlint-cli2-bin.mjs`,
using the repository configuration. The final four-file check (new clarification, change note,
this evidence and updated Scenario guide) passed with markdownlint-cli2 0.22.1: 0 errors.

## Acceptance and history

The [previous rolling repair evidence](796-797-rolling-admission-overflow.md) and all earlier
linked evidence remain unchanged. Git status/diff is unavailable in this captured source:
`git status --short` reports “not a git repository”; no repository was initialized.

The complete unchanged local fast gate is **NOT RUN by this implementer**; the native pipeline
owns it. Independent exact-successor build/private staging, `ImplementationTestEvidence`, required
bugs/correctness review and evaluator acceptance are **NOT AVAILABLE in this role**. Their results
must establish resolution of the recorded major; these implementer passes do not establish
acceptance or override the predecessor's failed Review. No broad convergence Review was started.

No publication, external messages/issues, credentials, machine configuration, production activation,
live recovery monitor changes, external build target or gate/limit/contract weakening occurred.
The explicit no-external-issues instruction governs this continuation. Issue 799 remains separate
owner design work; this role does not change captured model or Provider bindings.
