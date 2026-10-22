# CI fixture profile and shard experiment

## Decision

Keep the common-Store `task_starters` aggregate and the exclusive `light_strategy`
test unchanged. The prior split of the eight starter operations into separate
repositories and Stores lost cross-Task and repeated-review coverage. It is
rejected. This change adds only a read-only, path-limited pull request experiment;
production `.github/workflows/validate.yml` continues to run the complete gate.
The three measured runs below are exploratory, not performance acceptance or a
reason to promote sharding.

## Stage 1: captured process profile of original source

The captured source was commit `3aae93c98f1101fa64703f9b7dd50f20cbbdb5f1`,
Snapshot `sha256:8ffdbbfd7036642cafd0ceca9351f05e42a803e517b69bb30ded036aa4b427dc`.
Both profiled tests passed **before** any fixture edit. The environment was Linux
x86_64 with two logical CPUs, pinned Rust 1.88, a previously built native `af`
test binary, and process-only `strace`. Diagnostics overlapped other work, so
these are stage-attribution observations, not controlled baseline latency or
evidence of a candidate speedup. The trace digests bind the two process traces.

| Fixture and exact test selection | Test wall | Process trace SHA-256 | Failures |
|---|---:|---|---:|
| Common Store: `task_starters::emitted_starters_validate_and_execute_without_credentials_on_one_common_runtime` | 394.79 s | `c2e017f8e34f222df4b08be840f9bf4f9817f1588a3b97abccc4cc6d393360b2` | 0 |
| Exclusive `self_optimizer::light_strategy_generates_one_candidate_without_exposing_source_to_author_workers` | 493.53 s | `2cac24c708ea0ff3393e673d26a0c4c20d0f5df0ae7221109ea484c8dc03feb7` | 0 |

Exact captured commands, run from the baseline build directory:

```sh
strace -f -ttt -T -e trace=process -o ../../artifacts/af-ci-optimize-20261002/new-run-1561/baseline-process.trace ../../scripts/afactory-dev-env /srv/openclaw/.cache/afactory/review-target/debug/deps/it-d1168c0a2580cada --exact task_starters::emitted_starters_validate_and_execute_without_credentials_on_one_common_runtime --nocapture
strace -f -ttt -T -e trace=process -o ../../artifacts/af-ci-optimize-20261002/new-run-1561/light-process.trace ../../scripts/afactory-dev-env /srv/openclaw/.cache/afactory/review-target/debug/deps/it-d1168c0a2580cada --exact self_optimizer::light_strategy_generates_one_candidate_without_exposing_source_to_author_workers --nocapture
```

| Captured stage | Calls and durations | Nested Git work |
|---|---|---|
| Common Store | `catalog init` once (0.79 s), `catalog test` once (3.61 s), six `task plan` calls (4.40–5.58 s), six first `task run --execute` calls (14.19–98.14 s), six replays (0.47–1.68 s), and two reviews (21.98 and 51.75 s) | 247 Git subprocesses, 16.13 s cumulative nested process time |
| Exclusive light strategy | `self optimize` calls (21.48–33.30 s); four first `task run --execute` calls (37.28–96.05 s); two `task run --execute` replays (1.17–1.55 s); one `task start --execute` (15.28 s) | 188 Git subprocesses, 9.27 s cumulative nested process time |

The nested process times overlap parent processes and must not be summed into
wall time. Some nonzero CLI exits are asserted negative cases; both test
processes exited zero. Process-only tracing does not isolate Python startup,
fsync, worker execution, or receipt persistence. It gives no basis to shorten
sleeps, leases, deadlines, or process checks.

The previous attempt in an incomplete local registry could not compile the
baseline or candidate: offline Cargo lacked `serde`, and the network attempt
stalled on crates.io DNS. Its metadata failures were not test failures or
performance observations. The supplied external testing report used an older
source and Cargo-runner comparisons on a Mac; it does not establish a current
Linux nextest gain. The shared-state coverage requirement led to preserving
the aggregate byte-identical to S0. No post-change fixture speed comparison
exists because the fixture did not change.

## Experiment workflow and manifest proof

`ci-shard-experiment.yml` runs on a same-repository `pull_request` only when
the workflow, helper or this protocol changes. It uses `contents: read`,
checkout without persisted credentials, and no `pull_request_target`. Each
job checks out and records the exact PR head SHA. [GitHub documents the
default-branch requirement for `workflow_dispatch`](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow);
that event is not the premerge trigger. Opening a PR can trigger the first run;
subsequent **Re-run all jobs** actions retain the same head SHA while the PR
is unchanged. Do not merge merely to run this experiment.

Each attempt contains three variants on Linux with the pinned toolchain,
nextest, repository cache setup and four nextest threads:

| Variant | Build and execution |
|---|---|
| `single` | One `cargo test --locked --no-run`, then the complete nextest `ci` selection. |
| `archive-build` + two `archive-shard` jobs | One checked nextest archive, artifact upload and download, then two count partitions. |
| Two `rebuild-shard` jobs | Each runner builds independently, then executes one count partition. |

The compiling arms take a JSON sccache snapshot immediately after their build,
before manifest listing or long tests. Distinct arm and phase filenames retain
the raw server response plus validated requests, hits and misses. The helper
uses `sccache --show-stats --stats-format json`; its fields follow sccache's
[`ServerInfo` and `ServerStats` source](https://github.com/mozilla/sccache/blob/main/src/server.rs).
The captured response has `stats.compile_requests`,
`stats.cache_hits.counts`, and `stats.cache_misses.counts`. Missing, malformed,
zero-request or zero-classified-compilation statistics fail the compiling job;
no hit rate is inferred from absent counters. The existing Cargo registry
cache action also retains its separate `cache-metrics.json` outcome. Hits and
misses may each be zero
when the other is positive. These counters are measurement evidence, not a
claim that future arms must have the same count. The focused fake-sccache test
checks collection order and failure behavior; it is not cache benchmark data.

Before test execution, each variant uses [`cargo nextest list --profile ci`](https://nexte.st/docs/listing/)
with the same `--run-ignored default` selection as `nextest run --profile ci`.
It records full, first and second manifests and verifies nonempty, disjoint,
exhaustive runnable selections and identical ignored inventories. The archive
shards verify the checked manifests again after download. The aggregate
compares the manifests from the two actual rebuilt runners against both full
inventories. `--no-tests fail`, `fail-fast: false`, and the aggregate's four
required job results preserve zero-test rejection and failed or cancelled shard
outcomes. Count partitioning balances test counts, not runtimes. The existing
nextest exclusive override continues to apply inside the shard containing
`light_strategy`.

## Matched-run measurement protocol

Run at most 20 workflow attempts on one exact PR head, first aiming for three
complete matched runs and then up to ten if useful. Use sequential **Re-run
all jobs** on that same head. Key each row by `(run_id, run_attempt)` and match
`GITHUB_RUN_ATTEMPT` in `af.ci-step/1` records. For every attempt, obtain its
own `run_started_at` from `/actions/runs/{run_id}/attempts/{attempt}` and job
and step timestamps from `/actions/runs/{run_id}/attempts/{attempt}/jobs`.
`run.created_at` remains anchored to the original run across reruns and is
not a valid origin. The default jobs endpoint can return only the latest
attempt. Retain the API responses, checked `head-sha.txt`, manifests, JSONL,
JUnit, CPU/toolchain files, cache snapshots, failures and cancellations.
`af.ci-step/1.commit` may contain GitHub's synthetic PR merge SHA;
`head-sha.txt`, asserted in every job, identifies the measured source.

An attempt is comparable only when all build arms have a measured cold/warm
classification and comparable compile-phase sccache hit rates. Retain and
label all other attempts but exclude them from matched medians. Concurrent
cache writes may prevent comparability even when the starting classification
agrees. Never turn missing or zero counters into a 100% hit rate.

The observed single latency spans that attempt's `run_started_at` to its
`test` completion. Archive latency spans the same origin through archive
build, checked-archive upload, and the later shard `test` completion. Rebuild
latency spans the origin to the later rebuild shard `test` completion. These
spans include queue, dependency waiting, setup and transfer. The archive and
rebuild critical path uses the **maximum** shard completion timestamp, never
the sum of two shard durations. Separately sum active job durations through
test completion as a runner-occupancy proxy; this is not billed minutes.
Record checked-archive upload and both downloads as separate API step durations
and keep them included in archive operating cost. Exclude retention uploads
and the aggregate job from all three operating comparisons.

All `record` and `verify` invocations carry `manifest-proof` labels in
`ci-step.py`, including the unsharded arm's three listings and verification.
The compile-cache snapshot has its own label and remains part of measured
operating latency. For an adjusted comparison, subtract only the measured
manifest-proof duration on each critical chain, then recompute the maximum
shard completion. Retain archive upload/download, extraction, builds and
failures in both raw and adjusted results. Show raw and adjusted numbers
alongside each run; state whether a result is observed or adjusted. Calculate
medians and spread only over complete successful matched runs, reporting how
many were excluded and why. Do not report P90 from a few runs.

## Completed exploratory attempts and remaining Demands

At checked-out head `7406aaf45ac872a45c2faa77b77b74cdff384bb1`,
[experiment run 37019044401](https://github.com/PakhomovAlexander/afactory/actions/runs/37019044401)
had three attempts with all 21 jobs passing. All runners reported four CPUs.
The normal [PR CI run 37019044579](https://github.com/PakhomovAlexander/afactory/actions/runs/37019044579)
passed Linux check, lint and container probes. Release-only macOS and signing
gates were not run.

| Attempt | Single | Two rebuild shards | Archive plus two shards |
|---|---:|---:|---:|
| 1 | 816.63 s | 529.95 s | 591.09 s |
| 2 | 786.28 s | 446.37 s | 575.68 s |
| 3 | 805.31 s | 615.15 s | 575.71 s |
| Median | 805.31 s | 529.95 s | 575.71 s |

Subtracting only manifest-proof time on each critical chain gives:

| Attempt | Single adjusted | Rebuild adjusted | Archive adjusted |
|---|---:|---:|---:|
| 1 | 814.93 s | 528.54 s | 572.92 s |
| 2 | 784.62 s | 444.71 s | 550.34 s |
| 3 | 803.70 s | 614.00 s | 550.40 s |

Runner occupancy, the sum of relevant job active durations rather than billing,
was 13.60/13.07/13.42 min for single, 16.65/14.58/17.70 min for rebuild,
and 15.10/15.45/15.13 min for archive. Observed rebuild latency ranged
446.37–615.15 s, archive 575.68–591.09 s, and single 786.28–816.63 s.

The paired observed reduction median was 286.68 s (35.1%) for rebuild and
225.54 s (27.6%) for archive. The median runner-occupancy overhead was 22.4%
and 12.8%, respectively. The three archive uploads took 18/12/12 s; paired
downloads took (39, 7)/(7, 32)/(9, 18) s. They remain in the raw and adjusted
latencies. Three samples support neither P90 nor a production speed guarantee.

All three single arms reported **zero post-test sccache requests, hits and
misses**, while other build arms reported 175 hits and zero misses. An idle
server reset is plausible but unproven. Equal configured cache and exact SHA
do not prove equal effective cache state. These attempts are useful exploratory
latency evidence, but **do not satisfy performance acceptance**. The updated
compile-phase telemetry requires new controlled paired attempts. All prior
benchmark Demands remain open; production sharding remains disabled. The raw
attempt APIs, artifacts, complete manifests, JUnit, timestamps and analysis
are retained under `artifacts/af-ci-optimize-20261002/new-run-1561/benchmarks`.
A later separately reviewed change backed by controlled paired evidence is
required before altering `validate.yml`. Full exact-head GitHub CI, including
release and signing gates, remains a separate publication requirement.

## Disposition of prior review findings

| Prior finding | Disposition |
|---|---|
| Workflow dispatch could not run before merge | Repaired with a read-only, path-limited `pull_request` trigger and exact head checkout; dispatch retained. |
| Split starter test lost common Store coverage | Rejected the split; original aggregate remains byte-identical to S0. |
| Fixture edited without profiling | Captured original-source process profile above was inspected before the decision; no fixture edit was made. |
| Manifest proof and archive transfer skewed comparison | Proof steps are timed in every variant; transfer durations come from the attempt-specific GitHub steps API. |
| Rerun latency used workflow creation | Each attempt now uses its own `run_started_at` and attempt-specific jobs endpoint. |
| Manifest proof used nextest's default profile | Listing explicitly uses `--profile ci`, like execution. |
| Light-strategy stage summary omitted replay calls | Four first runs, two replays and `task start` are all accounted for above. |

The prior Opus report and its Demands remain predecessor evidence, not a clean
Review or closed Demand. Independent full main-to-final review and native
acceptance checks follow this implementation.

## Local checks on this repair

The earlier PR seed passed `cargo fmt --all -- --check`, 21 preflight checks,
YAML parsing and mock manifest cases. Its offline miniature Rust crate passed
full and partition proof from both live binaries and a nextest archive. Those
checks were plumbing evidence, not a performance benchmark. For this scoped
follow-up, the standard-library `scripts/test-ci-shard-experiment.py` passed
15 focused checks, `scripts/test-task-preflight.py` passed 21 checks, Python
AST parsing passed, and local YAML parsing confirmed the five experiment jobs.
No Rust source or fixture changed, and no local Rust compilation was run for
this follow-up. The full independent main-to-final Review and native acceptance
gates remain separate from these local checks.
