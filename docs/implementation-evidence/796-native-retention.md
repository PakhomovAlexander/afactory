# Issue 796: native retention continuation

Implementer evidence for the bounded retention continuation from captured Snapshot
`sha256:cb96dc032873437d4f2ad3c40ad4b6bf59e358bdc75e5892d61a5f12ecef4a1d`.
The preceding candidate and its still-running gate/review are not represented as accepted.
Historical [recorded defects](796-797-recorded-defects.md), [first implementation](796-797.md)
and [follow-up](796-797-followup.md) retain their original failures and limitations.

The concrete retention finding is
`sha256:00f00587e50d64a7482e2079a64ca6b644b4e18fdd75c7a0684ceeca01fa0775`, from reviewer
`sha256:167088ef72702fd05395174b7e20b938275e0ac2c25896deb6529c7706a4270c`.
This continuation implements the [native retention/capacity decision](../adr/2026-11-11-bound-native-ant-retention-and-settlement-capacity.md)
in the existing owned journal and keyed read index. It does not raise the 16 MiB journal limit or
change the machine Storage Budget. Current Store/state/inspection/list generations are 8/7/2/7;
current readers refuse the preceding shapes, and schema registration/parity changes with writers.
Raw Markdown, scheduler startup/stop serialization, daily and interval schedules, ordinary generic
outputs, manual identity/leases and separately captured execution authority remain in scope.

## Deterministic sustained evidence

The native Store tests use an inert private Host and controlled Unix milliseconds, with real SQLite
immediate transactions and the unchanged real `MAX_BYTES = 16 * 1024 * 1024`. No smaller test limit
stands in for the real failure point. Uncompacted byte totals count each distinct new receipt/settled
Run once; they do not sum repeated snapshots. Batching the long held test changes only SQLite
checkpoint frequency, while applying native compaction on each controlled tick.

- Mixed fixed-rate one-second occurrences with deliberately late ticks, skipped busy/missed ranges,
  successful, failed and cancelled Runs: 600 controlled driver ticks across 1,200 seconds, more
  than 300 unique admissions, **41,018,210 bytes** of uncompacted history, final serialized World
  **1,286,448 bytes**, SQLite file **3,313,664 bytes** in the recorded sustained run. The test proves
  continuing admissions, no duplicate occurrence, replay/restart, latest named `report` availability,
  explicit old-output pruning, output-index deletion, and cancellation after crossing the old bound.
- A native human decision remains unchanged across **102,400** busy one-second occurrences:
  **21,998,590 bytes** of uncompacted skipped history, final World **13,214 bytes**, SQLite file
  **45,056 bytes**. The decision request, Run/execution identity and zero dispatches survive restart;
  cancellation still works. Recent receipts plus the protected original receipt remain inspectable.
- Known native Task charges across 240 terminal outcomes retain exact cumulative retired-plus-live
  totals. Later completed/no-effect Runs compact around an unresolved historical hole without pruning
  it. The original known 65 plus unknown 35 reservation survives time/window expiry; native
  reconciliation to 73 retains Run/execution/intent identity and dispatch count. A separate
  100-token Ant refuses another occurrence a day later while its 65+35 unknown reservation remains.
- A rolling-bucket oracle compares the production merge against the uncompressed time-window sum
  at every boundary; merged buckets never undercount and eventually expire. Decimal u128 buckets
  preserve exact sums independently of the bounded admission comparison.
- A native Task response lease survives other admission refusals and a visible `skipped_capacity`.
  A real serialized **7 MiB** result then settles successfully, with exact charge 73, one original
  intent and no second effect identity. Other already-admitted Runs can still cancel.
- A large completed native result whose full roadmap projection cannot fit is explicitly pruned:
  Done intent, target, body digest and exact charge 17 remain; Scenario failure and deferred goal
  cannot imply successful completion or available body evidence. Restart validates the same native
  accounting. No open effect or unknown reservation is removed.
- Failed fresh planning without a recorded execution target retains its original Prepared intent,
  inputs, unknown Task charge and complete-loop reservation, while releasing unused full-result
  space. It cannot receive a Task result through that stopped driver. This preserves original
  template-refusal and source-provenance behavior under shared-Store capacity admission.

These are implementer tests, not independent sealed-candidate `ImplementationTestEvidence`.
The latest measured sizes can differ slightly with private path lengths; tests assert the actual
bound, counts, accounting and behavior, not one machine's byte spelling or elapsed performance.

## Execution

Darwin/arm64, pinned Rust 1.88.0 selected on PATH; Cargo `--offline --locked`, checkout-local
`target/`. `CARGO_HOME=$PWD/target/worker-cargo-home` contains only links to the existing registry
cache; no credential/config files were copied. `TMPDIR=$PWD/target/worker-tmp` is private writable
scratch. Native process tests use `crate::common::af()` and private HOME/XDG roots.

| Final command | Exit | Result |
| --- | ---: | --- |
| `cargo test --offline --locked -p af-scenario --lib -- --nocapture` | 0 | 72 passed, including nine new retention/capacity tests, exact accounting, schema/replay and controlled scheduler stop race; 129.67s on the shared host |
| `cargo test --offline --locked -p af --test it -- scenario:: scenario_performance::` | 0 | 34 passed; fresh Task, performance/roadmap, daily/interval, 1s process, generic outputs, raw Markdown and watch/recovery regressions; 123.03s |
| `cargo test --offline --locked -p af --bin af shipped_schemas -- --nocapture` | 0 | 4 passed; exact current registry, help/ID parity and unsupported-ID diagnostics |
| `cargo clippy --offline --locked -p af-scenario -p af --all-targets -- -D warnings` | 0 | Final source and test targets lint cleanly |
| `cargo fmt --all -- --check` | 0 | Final Rust formatting |
| `python3 -I scripts/adr.py list` | 0 | New dated decision discovered without editing an index |
| `python3 -I scripts/test-changelog-notes.py` | 0 | 13 passed |
| `python3 -I scripts/changelog-notes.py --check` | 0 | Change-note format accepted |
| Cached `markdownlint-cli2-bin.mjs` on all six touched Markdown documents | 0 | 0 errors, with repository configuration |
| `python3 -I scripts/measure-ant-intervals.py --af target/debug/af --seconds 4` | 0 | Fresh private finite scheduler/watch measurement below |

## Short final-binary measurement

The existing recipe created a disposable private HOME/XDG tree and owned Store, registered only
inert weather/library Ants, then explicitly enabled those fixtures. Each produced three unique
successful zero-dispatch Runs and named `report`/`empty` output. It installed no service, contacted
no sensor or external adapter, used zero model allowance, and cleaned only its own temporary root.

Final debug binary SHA-256:
`7077a21e5ad47715b8d4bedad0b29f7bc4da71ea777aa47dfd0181103afc0706`.

| Mode | Child exit | Wall seconds | User seconds | System seconds | CPU, one core | Package/interrupt wakeups | Sample seconds | Journal growth bytes |
| --- | ---: | ---: | ---: | ---: | ---: | --- | ---: | ---: |
| idle | 0 | 4.047150 | 0.034108 | 0.020117 | 1.3398% | 0/3 | 3.758561 | +4096 |
| one_second | 0 | 4.054415 | 0.213315 | 0.042201 | 6.3022% | 0/3 | 3.760375 | +73728 |
| watch_one_second | -2 | 4.009725 | 0.012240 | 0.009420 | 0.5402% | 0/3 | 3.755632 | +0 |

The watcher was intentionally interrupted by SIGINT (`wait4` -2, shell convention 130), retaining
its valid atomic Markdown snapshot. The recipe itself exited zero. CPU is owned-child `wait4`
accounting; macOS wakeups are unprivileged `proc_pid_rusage` v2 samples before exit, not host-wide
physical wakeups. These short debug runs shared the machine with compilation/testing and are not
production latency, release-comparison or hard real-time claims. Idle means two registered disabled
fixture Ants. Watching added zero journal bytes; the separate sustained tests prove retention
beyond the prior bound. Intermediate measurements are not substituted for this final binary.

## Development failures retained as history

- Initial 63-case domain run: 62 passed, one rolling-budget fixture failed because it labelled a
  Task failure definitive but left its charge unknown while expecting age-based release. The
  fixture now supplies its known 100-token settlement, preserving its existing release assertions;
  new unknown-usage tests assert indefinite reservation, including after terminal failure.
- First new-test compiles failed for `usize`, then `u64`, as SQLite `FromSql` count types. Use the
  supported signed count type; no bound or assertion was weakened.
- First new retention run: three passed, two new-test assertions failed. The inert skipped step
  needs a subsequent native prepare to finalize its frame; a literal root output is legitimately
  available while another step holds. Fixtures/assertions were corrected to native behavior.
- First expanded native-process build failed because renamed schema files still had old numeric
  registrations in `shipped_schemas.rs`. All current registrations were updated together.
- Adding retired mode provenance initially failed the Rust borrow check. Compute next due time
  before the mutable summary borrow.
- Expanded 34-case Scenario process suite initially had 32 passes and two capacity regressions:
  fresh-chain replay and the shipped performance loop. A failed, unmapped fresh planning intent
  unnecessarily retained a full Task-result lease. The structural planning-only rule above fixes
  this without dropping uncertainty or using diagnostic prose as authority. Both cases passed in
  isolated repair runs; final suite results are recorded separately.
- The first direct Markdown library invocation exited zero without running the CLI; it is not a
  lint pass. The actual CLI initially found an existing extra blank line in the touched result
  README. Removing that formatting defect produced a clean actual CLI run.
- A Git diff command was unavailable because this captured SourceTree has no Git metadata. No
  publication, live worktree or machine state was created to compensate.

## Acceptance authority and boundaries

The unchanged native local fast gate is **NOT RUN by this implementer**; the pipeline owns it.
Independent correctness build/private staging, sustained tests and `ImplementationTestEvidence`,
required bugs review and evaluator conclusion are **NOT AVAILABLE in this role**. They must check
the exact sealed candidate. Focused passes do not turn any earlier failed complete Review or gate
into acceptance. No broad convergence review or follow-up Review Task was started.

No external messages/issues, credentials, publication, machine configuration, production service
activation, live recovery monitor change, external build target, auxiliary ledger, or budget/gate
weakening occurred. No external issue was filed under the explicit prohibition on external
messages and credential use. The captured model/Provider roles and local-only gate authority are
unchanged. Issue 799 remains separate owner design work.
