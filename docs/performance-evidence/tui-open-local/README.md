# tui_open: the first real local run of the browser case

The retained evidence of
[ADR-0259](../../adr/0259-bound-bare-afs-first-browser-frame.md): the laboratory case
`tui_open`, both registry instances, run for real on a macOS arm64 host in the named
`macos_arm64_local` environment, trusted local mode, as
[`p8-local`](../p8-local/README.md) describes the workflow. The products were built from source
offline, every sample ran under the authority supervisor around the PTY driver
`fixtures/performance/cli/tui-open.py`, and the oracles judged every sample.

It is functional local evidence of the case. It is not a calibrated result, a shipping-budget
demonstration (the budget `tui_browser_under_200ms` stays `pending_calibration` and names 20
shipping samples per instance on its own environments), a Linux or contained result, or a
hosted run. The comparison is same-source (A/A): baseline and candidate are one commit and one
build, the tree whose browser paints before any Store is read (ADR-0240).

## What was measured

- **Source.** Every file of this Task's tree before sealing, but its build `target/`, committed
  once into a run-owned Git repository (`$WORK/repo`): commit
  `79d99e24ce6036d696fe33908382e0b67f8bb38c`, Git tree
  `66e5d7376eb7fb335e34b8afc4cae80d65b0a8bd`, 2,868 files, protected harness
  `sha256:d2f6184f0c98a0a8b1461ab8cdbf9ef94512c72962af05eba75cd0b471dfc548`. Files changed
  after the run are listed under "Later differences".
- **Host.** Apple M3 Max, 14 cores, 36 GiB, macOS 27.0.1, the laboratory and the driver on
  Python 3.9.6 (`/usr/bin/python3`, so `{python}` named it), rustc 1.88.0. The host ran other
  Tasks' gates beside this run: load averages 21 to 40 around the shard.
- **Dependencies.** A run-owned Cargo home holding a copy of the host's Cargo registry, no
  credentials; every build ran `--locked --offline`.

## Steps and exits

[`steps.jsonl`](steps.jsonl) holds each step's argv, exit status and wall time, with the run's
work directory written `$WORK`. No step was retried.

| Step | Command | Exit | Seconds |
| --- | --- | --- | --- |
| capabilities, list | `cli.py capabilities`, `cli.py list` | 0, 0 | 0.8, 0.5 |
| plan | `cli.py plan --trigger manual --scope partial_scope --tier nightly --environment macos_arm64_local --shards 1 --pairs 12 --seed 20261028 --run-id tui_open_local --case tui_open__registry_empty --case tui_open__registry_stores_20x10000`, base = head = control = authority = the measured commit | 0 | 0.7 |
| build | `cli.py build --plan run.json --cargo-home $WORK/cargo-home` | 0 | 255.4 |
| run-shard-0 | `cli.py run --shard 0 --trusted-local --scratch $WORK/scratch` | 0 | 235.8 |
| aggregate | `cli.py aggregate --run $WORK/run` | 4 | 0.5 |
| report | `cli.py report --run $WORK/run --output $WORK/report` | 4 | 0.5 |

The plan takes 12 pairs, the least a plan admits: `--pairs 5` was refused with exit 2 and
`refused (invalid_plan): pairs: an even count within 12..200`
([`attempt-1/steps.jsonl`](attempt-1/steps.jsonl) keeps that refusal's plan step). Plan digest
`sha256:9ebb97509699fd7503ff4cc54061051c9b2c9b3b37c8bda9cda917e2a23f8635`; `af`
`sha256:edcf6f2c05a90cae282221b38123f2b1aded82128a94360c39f8562a37042093`, probe
`sha256:e2b51057ddc57d310cc14cf11d10dcc5273e2618c7cb4ec5b0cad40d394385aa`, supervisor
`sha256:191ca6d056fe3d2829eb12ee62a7ac76bba49699a9a728f5a57ae358ce6d2d5a`.

## Results

Shard 0 completed with 76 of 76 planned records and no failed sample (samples
`sha256:fb4c15654ce465e4ba57e940a3ab9e65175f0b7889e402886dcde7b137e9bb4f`, shard record
`sha256:52addf6d158a60cf5a366b9d00dceb3c5e85be5248d60bd1ff0d6fdf6dbce6d1`). Every sample of
both instances recorded `wall_ms` (the driver's `browser_ms`) and `splash_ms`, both with the
scope `probe_reported`; the report's evidence copy of the samples carries both.
[`results.json`](results.json) lists every record in planned order with both values.

| Instance | Side | Measured | `browser_ms` min, median, max | `splash_ms` min, median, max |
| --- | --- | --- | --- | --- |
| `tui_open__registry_empty` | baseline | 12 | 7.464, 8.250, 12.726 | 6.788, 7.469, 11.171 |
| `tui_open__registry_empty` | candidate | 12 | 7.857, 8.378, 10.527 | 7.235, 7.457, 9.390 |
| `tui_open__registry_stores_20x10000` | baseline | 12 | 11.092, 11.921, 34.514 | 7.007, 7.348, 24.408 |
| `tui_open__registry_stores_20x10000` | candidate | 12 | 10.930, 12.667, 18.730 | 6.796, 7.693, 15.521 |

All 48 measured samples are below 200 ms. The first warmup sample of the run, the first launch
of the freshly built binary, took 922.1 ms to its browser frame; the other seven warmups took
7.8 to 49.2 ms. With 20 Stores of 10,000 events registered, the browser frame comes about 4 ms
later than with none, and the splash not at all: the Stores are read behind the browser.

The verdict is `incomplete` (exit 4): both A/A cases are `inconclusive` at 12 pairs on this
loaded host, whose intervals exceed the policy's tolerance (relative 0.05, absolute 1 ms), and
a partial-scope, proposed-policy run cannot satisfy the required check. No budget was judged:
the browser budget names nightly and release samples of a shipping recipe, not this run.

## Attempt 1: the registry setup did not finish

The first run measured an earlier pre-sealing source (commit
`9dfa317c8e31d37e0fb47493b4842e19638cc165`, plan
`sha256:607bfe03ad60b590a2e48bbb592dfa937c388d890a3d227a99649dd93f276f3f`) whose registry
setup recorded each Store through `af task list --state`. The build and the shard exited 0 and
the shard completed 76 of 76 records, but all 28 samples of
`tui_open__registry_stores_20x10000` failed with `launch_failure`: "the side's fixture setup
failed, so nothing was measured: it exited 1". The setup's last check found no
`$XDG_STATE_HOME/af/stores.toml`: `af task list --state` only reads its Store and records
nothing, while every command that resolves a `--state` Store through
`task_execution::state_path` records it (ADR-0144). The setup now registers each copy through
the side's own `af task gc --older-than 36500 --keep 1000 --state DIR` preview, which records
the Store and writes nothing else; `storage_budget::a_task_gc_preview_registers_its_state_store_and_a_listing_does_not`
holds that rule. The empty instance's 24 measured samples passed, 8.8 to 29.5 ms.
[`attempt-1/`](attempt-1/) keeps its steps and every record.

## Later differences

After the run these paths changed, and nothing else: `docs/README.md`,
`docs/adr/0259-bound-bare-afs-first-browser-frame.md`,
`docs/design/continuous-performance.md`, `docs/performance-acceptance.md`,
`changelog.d/perf-tui-open-target.md`, `fixtures/performance/README.md`, this directory, the
docstring of `scripts/performance/perflab/capability.py`, the driver
`fixtures/performance/cli/tui-open.py`, the tests `scripts/performance/tests/test_tui_open.py`,
`test_runner.py`, `test_diagnostics.py` and `support.py`, the diagnostic child run
`scripts/performance/perflab/diagnostics.py` (it reads the driver's unconfirmed exit as the
shard does; this run took no diagnostic), and the protected-harness inventory
`fixtures/performance/kernel-measure/code-policy-measures.toml` and `inventory.json`,
re-rendered for them. No registry, setup, runner or product code changed. The driver did
change after the run, in its cleanup and its reading of the terminal, not in what it
measures: the review rounds after it made the driver find a process that left the browser's
session by a token in its environment and fail closed on a listing it cannot confirm, count
what its settlement finds, end the session when its exit watcher is refused and read the
terminal through every wait of its cleanup (an exiting session leader's close of its terminal
waits on macOS until its pending output is read), hold a
cancellation that lands during the settlement until the settlement is confirmed, keep the
first-byte and status-line arrivals in a `Screen` that a later clear erases, and read the
terminal through the browser's exit. The measurements above were taken with the driver as it
was at the run (its `browser_ms` and `splash_ms` are the same two reads, taken the same
way), so they do not measure the final driver revision; its regression coverage is the test
module's, not this run's.
