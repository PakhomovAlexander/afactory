# Issues 796/797: rolling admission overflow repair

Fresh implementer evidence for the bounded continuation of Task
`ants-796-797-final-help-repair-20261111`, original Round 1. The captured source is
`sha256:9700d02b3564037c8041aeb3d3b615fa9cfc9a6a4dce0f24723e73be4893c7eb`;
the finding names inherited candidate
`sha256:18ad56a727b3fb09599b0b4e1c0ffa815bf9e123ab50bf240223ab3360e61081`.

The recorded major is `bugs-1`, title “Rolling admission errors instead of skipping when retained
charges overflow u64”, reviewer result
`sha256:94b04e15bf2ac5058fae781082236e4fcc42dc4c79ac93ff76e66d3816974061`.
The requirements identify finding
`sha256:f2ef8bfb16fbfb8fadd5a7d63eea771071441b4b1340d77b8859bb58e3bd9758`;
the supplied native findings envelope is
`sha256:35ff4634ae707bc4ff6d122822b931630b9223551deef6030e45f43fd79342ff`.
The predecessor's verified label does not resolve this major.

## Change and deterministic regression

The retained-Run admission fold and prospective reservation now use saturating addition.
The [dated clarification](../adr/2026-11-11-saturate-complete-ant-rolling-admission.md)
implements the existing [retention contract](../adr/2026-11-11-bound-native-ant-retention-and-settlement-capacity.md).
Exact Run charges, decimal u128 cumulative/bucket accounting, unknown reservations, conservative
bucket merging and expiry remain unchanged. Persisted shapes and authority remain unchanged.

`retention::tests::rolling_admission_saturates_retained_and_retired_overruns_and_replays_exactly`
uses private native SQLite Stores, controlled one-second ticks and the inert native Task Host.
Three small reservations are admitted before native reconciliation records u64-maximum overruns.
One branch ages protected unknown receipts through paused ticks, then reconciliation retires two
charges (exactly 36,893,488,147,419,103,230 tokens) while retaining the latest charged Run.
The other branch retains all three Runs. Both cases assert:

- The tick succeeds with no refusal or admission, records `SkippedBudget`, and persists the
  advanced occurrence cursor and tick timestamp across a reopened Store.
- Retired buckets and cumulative charges remain exact; retired plus live charges total
  55,340,232,221,128,654,845 tokens across replay and subsequent expiry.
- Repeating the same tick does not advance the cursor again. Charges suppress admission just
  before their ordinary rolling boundary; the boundary allows a new Run without losing
  cumulative charges.

Before the implementation change, the new regression failed with exit 101 and
`{"overflow": "Ant rolling accounting overflow"}`. After the change it passed both branches.
The final focused selection includes an additional exact bucket-total assertion.

## Execution

Darwin/arm64, pinned Rust 1.88.0 on PATH, offline locked Cargo, checkout-local `target/`.
`CARGO_HOME=$PWD/target/worker-cargo-home` links only the existing registry cache;
`TMPDIR=$PWD/target/worker-tmp` is writable private scratch. No credentials or machine
configuration were read or copied. Commands below use that environment for Cargo builds.

| Command | Exit | Result |
| --- | ---: | --- |
| `cargo test --offline --locked -p af-scenario --lib rolling_admission_saturates_retained_and_retired_overruns_and_replays_exactly -- --nocapture` | 101 before fix; 0 after fix | Reproduced the recorded overflow, then passed both compositions |
| `cargo nextest run --offline --locked --profile ci -p af-scenario --lib -E 'test(rolling_admission_saturates_retained_and_retired_overruns_and_replays_exactly)'` | 0 | Final focused regression: 1 passed, 72 skipped |
| `cargo test --offline --locked -p af-scenario --lib -- --nocapture` | 0 | 73 passed, 0 failed; 119.90s, including rolling/exact/unknown accounting, retention/replay, interval scheduling and stop serialization |
| `cargo clippy --offline --locked -p af-scenario --all-targets -- -D warnings` | 0 | No warnings |
| `cargo fmt --all -- --check` | 0 | Final Rust formatting |
| `python3 -I scripts/adr.py list` | 0 | Dated decisions discovered without index edits |
| `python3 -I scripts/test-changelog-notes.py` | 0 | 13 passed |
| `python3 -I scripts/changelog-notes.py --check` | 0 | Change-note format accepted |
| Actual cached `markdownlint-cli2-bin.mjs` on the three new Markdown files | 0 | markdownlint-cli2 0.22.1: 3 files, 0 errors, repository configuration |

The Markdown command invoked the actual CLI directly with Node:
`node /Users/apakhomov/.npm/_npx/38d773fe1452947e/node_modules/markdownlint-cli2/markdownlint-cli2-bin.mjs`,
followed by the new decision, change note and this evidence file. It did not import a library.

The first formatting check reported the new comparison's line wrapping. `cargo fmt --all`
corrected it before final checks. Git status/diff is unavailable in this captured SourceTree:
`git status --short` reported “not a git repository”. No repository was initialized to compensate.

## Acceptance boundaries and history

Earlier evidence is preserved in [native retention](796-native-retention.md),
[help repair](796-797-help-width-repair.md), [process checks](796-797-process-checks.md),
[owner-authority gate](796-797-owner-authority-gate.md),
[recorded defects](796-797-recorded-defects.md), [first implementation](796-797.md), and
[follow-up](796-797-followup.md). Their original results are historical and unchanged.

The complete unchanged local fast gate is **NOT RUN by this implementer**; the native pipeline
owns it. Independent build/private staging and exact-successor `ImplementationTestEvidence`,
required bugs/correctness reviews and evaluator acceptance are **NOT AVAILABLE in this role**.
They must establish resolution of this major on the sealed candidate. Focused implementer
passes alone are not acceptance. No remaining in-scope major is known to this implementer;
independent reviewers must report any they find and the evaluator must refuse acceptance while
one remains. No broad convergence Review Task was launched.

No publication, external messages/issues, credentials, machine configuration, production
activation, live monitor changes, external target or gate/budget/contract weakening occurred.
The explicit no-external-issues scope overrides the repository's general raw-issue filing rule.
Issue 799 remains separate owner design work. This role does not change captured model or
Provider bindings.
