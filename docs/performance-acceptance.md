# Continuous performance laboratory: acceptance matrix

What the [design](design/continuous-performance.md)'s §15 acceptance requires, against what
exists on the source of package p8
([ADR-0186](adr/0186-fold-protected-paths-key-warm-targets-by-the-whole-snapshot-bound-measure-output-and-replan-bridge-runs.md))
and its closeout
([ADR-0187](adr/0187-bind-kernel-receipts-to-their-task-results-word-reports-by-what-was-admitted-and-retain-a-real-local-run.md)).
Nothing here is an activation, a calibrated sensitivity or a validated shipping speed: no hosted
run, cohort, cost run or shipping run has executed, and enforcement is advisory. One real local
same-source run of the documented workflow is retained in
[`performance-evidence/p8-local/`](performance-evidence/p8-local/README.md); it is functional
evidence on a shared macOS arm64 host, not a speed claim. The pull
request that delivers this source carries the `af task report` of its Tasks, and those reports,
not this page, are the record of what each Task's gate and reviews concluded.

## Columns

- **Implemented and tested.** Executable support exists and deterministic or local tests
  exercise it. Synthetic fixtures check arithmetic and orchestration only.
- **Measured, target unmet.** A real measurement exists and misses its target. There is none.
  The closeout's local run measured three cases once, same source against itself, with no
  product-speed or sensitivity target, so no target is claimed met or unmet.
- **Unavailable environment.** The capability is implemented, but the hosts available to these
  Tasks cannot run it, and the laboratory refuses with its reason.
- **Pending.** A multi-day, hosted, staging or operator action that no Task may take.

## Matrix

| Obligation (§15) | Implemented and tested | Unavailable environment | Pending |
| --- | --- | --- | --- |
| Correctness and contracts | Exact statistics, boundary and corruption fixtures, versioned closed contracts with schema parity (`scripts/test-performance.py`, `scripts/performance/schemas.py`), independent golden oracles, side-specific failure attribution; Python 3.9 and 3.11+ parser parity; diagnostic and calibration feature builds (`make lint`) | — | — |
| Execution | Frozen plans, build provenance, supervised paired shards, CLI fixtures with private HOME/XDG, sentinels and health; trusted local mode for discovery and nonspawning library cases. Executed for real in `macos_arm64_local` (ADR-0187): a partial-scope A/A plan of three critical cases, the CLI case `cli_discovery__command_version` among them, built offline and run as one healthy shard of 152 of 152 records, then reproduced and run again | Contained Linux samples: this Task's macOS arm64 host reports `linux_container` unavailable (no verified disk-bounded scratch). Docker Desktop 27.4.0 (arm64, overlay2) refused `--storage-opt size=2G` with exit 125 | Real paired bundles for every critical case on a Linux host with a verified scratch |
| Cost | `cost-report` (`af.perf-cost/3`) counts every retained execution, its failures and observed cold or warm compiled cache | — | Five real pull-request and five same-source comparisons, each cold and warm, on hosted runners |
| Detection and reliability | Cohort declaration, sealed parameters, injection, readiness and activation verification (`af.perf-readiness/6`, `af.perf-activation-verification/3`), bridges replanned from the bridged authority (ADR-0186) | — | The fixed five-day hosted cohort: 20 A/A, preparation, 30 injection and 10 power trials, then a reviewed activation |
| Diagnosis | Separate diagnostic child runs, Linux perf capture path, inferno differential SVG, default-off phase traces, optional samply, DHAT and Callgrind adapters with honest unavailability. A real `macos_arm64_trace` child run recorded four complete phase traces, linked from the measured report without changing its verdict (ADR-0187) | Linux perf and contained CLI captures need a Linux host with perf and a verified scratch. No perf, samply or inferno is configured on this macOS host, and none was installed | Real symbolized Linux baseline, candidate and difference profiles with per-case trace overhead |
| Product speed | `shipping-report` (`af.perf-shipping/3`): one target per product identity and environment group, maximum, median and nearest-rank p95 | Linux shipping environments are unavailable here. `macos_arm64_local` was sampled by the closeout's same-source runs, 20 pairs per case on a shared host: not a shipping demonstration | Twenty shipping samples per discovery command on named Linux and macOS, and per state command on Linux, checked against 50 and 200 ms |
| History and operations | Offline reports (verdict first, only failed or incomplete cases in the first view, measured wording from admitted records; ADR-0186, ADR-0187), content-addressed archive and collector, compatible history, release handoff with deduplication. The real run and its executed reproduction were archived and drawn from the archive as history of two runs in three compatible groups | — | Provisioned collection before artifact expiry, a hosted staging release handoff, and recorded ruleset and release-pin coexistence |
| Experiments | `cli.py measure` through the literal bootstrap. The guard folds case, a warm target is keyed by the whole Snapshot, and capture and logs are bounded (ADR-0186). Real kernel integration covers six tampered candidates and a controlled improvement, with `af.perf-kernel-receipts/3` receipts whose envelope identities, stream IDs and Task references the reader recomputes (ADR-0187) | — | Acceptance of that integration on the sealed source; a reviewed human change installing the staged policy and selecting the inner metric |

## Retained evidence and open observations

- **Defaults and bounds.** p8 read the registry and `perflab.limits` against design §14. Quick
  tiers run 2 shards and 2 jobs with 15 and 60 minutes; nightly and release run 4 and 4 with 45
  and 120. Pairs are 40 for cheap and 20 for other quick cases, 40 nightly. Each case has 2
  warmup pairs. Logs are 256 KiB, bundles 128 MiB, build artifacts 1 GiB and worktrees 40 GiB,
  builds take 20 minutes, and the free-disk floor is 10 GiB. The hard maxima are those of §14;
  the `state_projection` case declares a 120-s sample timeout, at the hard maximum. The kernel measure's capture
  uses the registry's 256-KiB sample log bound and the 4-MiB default build log.

- **p7's gate failed.** Task `perf-p7-20261021`, snapshot
  `sha256:6cf34d4250c9f3f09bc577b7b4a6b5f39a5609249adc8a3575da5fc1d86a7d2a`, receipt
  `sha256:889bf9e322fef0f941c0ac415addb6e1aad23cbdedcd72ee72c0a7e15adc3fac`: 2,144 of 2,145
  Rust tests passed, 14 skipped. 639 Python tests and the native kernel integration (452 s) also
  passed. `campaign_loop::committed_and_dirty_diff_subjects_execute_the_wired_change_set` saw
  `-1` with empty streams after 0.119 s. Its cause is not established. The helper now keeps the
  real `ExitStatus` and signal, and three isolated repeats on p8's source passed. Both semantic
  reviewers and the evaluator were suppressed; that is not a review pass.
- **p8's native kernel integration.** On p8's source before sealing, the integration passed in
  668 s on this Task's shared macOS arm64 host, whose other workloads included another AF gate.
  The harness had 450 protected files. The controlled improvement verified, with probe-region
  medians of 1510.11 ms against 0.09 ms. All six tampered candidates failed their first
  repetition before import or build, the case-variant one included at 452 files. Every comparison
  but the controlled one was inconclusive, and the exported `af.perf-kernel-receipts/2` passed
  its schema and reader. That is a disposable consumer's controlled slowdown, not a product
  speed claim. That `/2` reader compared only the IDs a document reported about itself; the
  closeout's `/3` reader recomputes them (ADR-0187).
- **p8's predecessor outcome.** Task `perf-p8-integration-20261021` (result
  `sha256:fbc790159f5c968aec69f2a982a90fda2b08c9e8182a6f59c6b6defd1ad82a83`) is three separate
  facts. Its full gate passed: 649 Python tests with 2 skips, 2,145 Rust tests with 14 skips and
  the native guarded integration in 691.652 s. Its reviews and the root found three report and
  receipt defects and the root's envelope-identity counterexample, which ADR-0187 repairs. Its
  goal evaluator failed it for the missing real workflow, which the closeout's local run
  supplies. The root's own native build of that source passed and its native test was cancelled
  on purpose on the superseded source: neither a pass nor a failure of the product.
- **The closeout's local run.** On the closeout's pre-sealing source, in `macos_arm64_local` on a
  shared Apple M3 Max with load averages of about 11 to 29: capabilities, list, a frozen
  partial-scope A/A plan of three critical cases, an offline build in 124 s, one shard of 152 of
  152 healthy records, a `pass` verdict that does not satisfy the required check, a linked
  phase-trace child run whose four traces are labelled `perturbing`, the report, an archive
  import, history, the report's own reproduction commands and an executed rerun, which also
  passed. A superseded first run of an earlier source passed too, but its rerun was
  `incomplete` under load averages near 80. Every command, exit, identity and output, and how to
  reconcile the measured source with the sealed one, are in
  [`performance-evidence/p8-local/`](performance-evidence/p8-local/README.md).
- **Hashing a Snapshot.** ADR-0186 no longer states a hashing time. One observation: the source
  key over that run's 1,906-file, 21.3-MB source took a median of 117.4 ms in five repetitions at
  a load average near 26. Nothing is claimed for larger Snapshots.
- **p6b's LEAK label.** One published nextest LEAK observation of
  `build_metadata::build_metadata_watches_existing_git_paths_and_current_branch` named no
  surviving process. Three isolated nextest repeats on p8's source showed no LEAK label, which
  says nothing about the historical process state.
- **The report's browser check.** An earlier Chrome 155 check filtered rows from 164 to 0 and
  back with unchanged report bytes, but Chrome exited 2 with `Teardown watchdog expired`. That
  is neither a clean browser pass nor an established defect, and a later run does not explain
  it. On p8's source, a synthetic sample report rendered by `report.write` was opened in
  headless Chrome 155.0.8059.85 over the DevTools protocol, with a fresh profile, the browser
  sandbox and the page's CSP. Its first section was the verdict. The filter showed 167 rows,
  then 0 for an absent text, then 167 when cleared. The three disclosures loaded closed, and a
  click on the first one's summary opened it.
  No console or CSP message appeared, Chrome exited 0 after `Browser.close`, and the report's
  bytes were unchanged. That is one local observation of a synthetic report, not a measured
  report's check. On the closeout's source, the same browser opened the real measured report twice,
  for the superseded first run and for the retained one, with the same outcome:
  the verdict came first, the filter showed 190 rows, 0 and 190 again, the 3 disclosures opened,
  no console or CSP message appeared and the bytes were unchanged. That page interaction
  completed. Separately, Chrome then exited 2 with `Teardown watchdog expired` after
  `Browser.close` and left no process; the shutdown outcome is the browser's, not the page's.

## Remaining blockers and next steps

| Blocker | Reproduce | Next step (owner) |
| --- | --- | --- |
| No verified disk-bounded scratch for contained Linux samples | `python3 scripts/performance/cli.py capabilities` reports `linux_container` unavailable | Run on a Linux host or hosted runner with a dedicated block filesystem, as [`performance-ci.md`](performance-ci.md) prepares it, and verify it with `--verify-scratch-enforcement` (operator) |
| No hosted run of `performance.yml` or `performance-activation.yml` | The workflows have no run history | An authorized operator configures runners and images and dispatches the workflows (operator) |
| No calibration cohort | No `af.perf-readiness/6` record from hosted originals exists | Declare a cohort, merge it and dispatch every member over five days on fresh hosted runners (operator, multi-day) |
| No cost or shipping measurement | `cost-report` and `shipping-report` have only synthetic inputs | Retain five pull-request and five same-source runs, cold and warm, and twenty shipping samples per command and environment (operator) |
| No staging release handoff, archive collection or ruleset coexistence | No staging release, collector or ruleset record exists | Provision staging credentials and the collector, then record ruleset and release-pin coexistence (owner) |
| Staged guarded measures not installed | `.af/code-policy.toml` holds no guarded measure | After the integration is accepted on the sealed source, a reviewed human change appends `fixtures/performance/kernel-measure/code-policy-measures.toml` and may select `wall_ms` (owner) |
