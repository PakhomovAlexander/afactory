# Issues 796/797: exact goal-claimed roadmap accounting

Implementer evidence dated 2026-11-12 for captured source
`sha256:f6d5bddbd798e29b80c09a3f93effc6371d7b328139484c979b6279e66967329`.
This continues the reviewed candidate
`sha256:5cc949a71388093d5d488ed71394c719d3e255f29d9134cf70904d7cc57c80b1`
and findings artifact
`sha256:a4fb61adc961f2afeb62e27141def25b7eb593b233de7e0594649e07edb6d2b7`.
The predecessor's failed evaluator verdict and
[earlier evidence](796-797-within-run-accounting.md) remain historical evidence.

## Repair

The [dated decision](../adr/2026-11-12-preserve-exact-roadmap-overruns.md) describes the
changed boundary. Roadmap progress actuals, accepted receipt sums, reconciliation and total/day/
rolling admission use checked u128 arithmetic. Durable actual totals use canonical decimal text;
configured allowances and reservation bounds remain unchanged. Independent unknown reservations
survive recovery of another overrun. Roadmap/list v3 and Store v9, including the ownership marker,
replace earlier persisted shapes without migrations. Proposal/revision authority stays v2.

The schemas now describe the existing exact u64 native individual charge range. The native
writer and reader already preserve those integer counters; this corrects their schema mismatch,
without raising dispatch budgets. Public roadmap totals use decimal text to exceed u64 exactly.
Progress schema definitions belong to the new roadmap schema; CLI registration, help, inspection,
Store replay and parity tests use the current versions together.

## Native regressions

`goal_claimed_recovered_overrun_persists_replays_and_refuses_admission` uses a private SQLite Store
and inert Host, through actual fresh Task planning, dispatch and runtime settlement. It settles
17 tokens, loses the next response and recovers that Task at u64 maximum. Before the repair the
nextest ci run failed at native reconciliation with `Roadmap charge overflow`:

```text
Summary [   0.082s] 1 test run: 0 passed, 1 failed, 74 skipped
```

After repair the same path persists **18446744073709551632** as canonical decimal actual spend.
The test reopens and validates the Store, checks exact charge, plan, intent, receipt and dispatch
identities, and verifies reservation release. Its second branch keeps the first Task's known
17 tokens and independently unknown remaining three-token reservation. Both branches reject a
later manual goal after ordinary window expiry, refuse a revision below cumulative commitments,
and durably record a later Ant roadmap skip without dispatch. Native compaction preserves the
protected goal-claimed Run and its exact progress.

`single_goal_charge_above_safe_integer_replays_and_releases_only_its_reservation` recovers one
Task at 9007199254740992, validates its schema and replay, releases its resolved reservation,
keeps terminal failure and refuses later dispatch. Schema tests reject the previous roadmap/Store
identities and numeric, noncanonical, negative and overflowing decimal totals.

The earlier native within-Run and retained/retired across-Run overflow tests remain intact.
Ordinary roadmap settlement, day/rolling/total windows, revision invariants, capacity, protected
retention, one-second scheduling and read-only Markdown output remain covered by the library and
Scenario CLI selections. No unvalidated Run or fabricated roadmap progress is injected by the new
recovery tests; the inert Host supplies typed Task plans, receipts and lost-response recovery.

## Execution

Pinned Rust 1.88.0, Darwin arm64, checkout-local `target/`. A private Cargo registry cache and
`TMPDIR=/tmp` avoid the inherited unwritable private home and temporary directory. No credentials
or machine configuration were read or changed. Initial cache/temp permission errors and an
incorrect binary selector occurred before execution; the corrected package filter reproduced the
native failure above. These setup failures are not passing evidence.

- `cargo fmt --all`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed without warnings.
- Complete af-scenario library plus Scenario CLI module under nextest ci: 110 passed,
  zero failed, 1006 skipped (247.854 seconds; one slow retention test).
- Focused nextest ci native recovery/schema and CLI roadmap/help/schema parity: seven passed,
  zero failed, 1109 skipped. Both public schema catalog commands were executed.
- `python3 -I scripts/changelog-notes.py --check`: passed.
- `python3 -I scripts/test-adr.py`: nine passed.
- `python3 -I scripts/test-changelog-notes.py`: thirteen passed.
- Actual `markdownlint-cli2@0.22.1` checks the Scenario guide, new decision, change note and
  this evidence under the repository configuration.

The final filtered nextest ci selection runs the complete af-scenario library and Scenario CLI
module again after all source/document edits; the worker reply records its exact summary and time.
The native Pipeline owns the full local fast gate and independent exact-successor judgments.
This implementer does not claim those future receipts, acceptance or measured improvement.

## Finding dispositions

- `bugs-1`: fixed at the native roadmap accounting boundary, with executable recovery and replay.
- `correctness-1`: fixed by the same complete repair and unknown-reservation/admission regressions.
- `evaluator`: underlying acceptance blocker repaired; the original failed verdict is retained.
  Successor gate, read-only reviewers and evaluator must judge the exact sealed successor; these
  implementer receipts do not substitute for their decision.

No publication, external messages/issues, machine changes, live recovery monitoring, Provider
calls, remote hosts, paid benchmarks or new Review campaign occurred. Issue 799 remains separate.
