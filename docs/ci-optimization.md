# CI fixture profile and shard experiment

## Decision

Keep the common-Store `task_starters` aggregate and the exclusive `light_strategy`
test unchanged. The prior split of the eight starter operations into separate
repositories and Stores lost cross-Task and repeated-review coverage. It is
rejected. This change adds only a read-only, path-limited pull request experiment;
production `.github/workflows/validate.yml` continues to run the complete gate.
No measured speedup or sharding promotion is claimed.

## Stage 1: captured process profile of original source

The captured source was commit `3aae93c98f1101fa64703f9b7dd50f20cbbdb5f1`,
Snapshot `sha256:8ffdbbfd7036642cafd0ceca9351f05e42a803e517b69bb30ded036aa4b427dc`.
Both profiled tests passed **before** any fixture edit. The environment was Linux
x86_64 with two logical CPUs, pinned Rust 1.88, a previously built native `af`
test binary, and process-only `strace`. Diagnostics overlapped other work, so
these are stage-attribution observations, not controlled baseline latency or
evidence of a candidate speedup. The complete captured profile is part of the
Task requirements; the trace digests below bind the two process traces.

| Source | Environment and fixture status | Timed repository tests | Failures |
|---|---|---:|---:|
| S0 | Linux x86_64, 2 CPUs, Rust 1.88, built test binary; original shared-Store and exclusive fixtures | 2 profiled below | 0 |
| This repair | Same local 2-CPU sandbox and unchanged fixtures; experimental workflow and helper added | 0 controlled candidate fixture timings | 0 local plumbing-check failures; focused native Rust check pending |

| Fixture and exact test selection | Test wall | Process trace SHA-256 | Failures |
|---|---:|---|---:|
| Common Store: `task_starters::emitted_starters_validate_and_execute_without_credentials_on_one_common_runtime` | 394.79 s | `c2e017f8e34f222df4b08be840f9bf4f9817f1588a3b97abccc4cc6d393360b2` | 0 |
| Exclusive `self_optimizer::light_strategy_generates_one_candidate_without_exposing_source_to_author_workers` | 493.53 s | `2cac24c708ea0ff3393e673d26a0c4c20d0f5df0ae7221109ea484c8dc03feb7` | 0 |

Exact captured commands, run from the baseline build directory:

```sh
strace -f -ttt -T -e trace=process -o ../../artifacts/af-ci-optimize-20261002/new-run-1561/baseline-process.trace ../../scripts/afactory-dev-env /srv/openclaw/.cache/afactory/review-target/debug/deps/it-d1168c0a2580cada --exact task_starters::emitted_starters_validate_and_execute_without_credentials_on_one_common_runtime --nocapture
strace -f -ttt -T -e trace=process -o ../../artifacts/af-ci-optimize-20261002/new-run-1561/light-process.trace ../../scripts/afactory-dev-env /srv/openclaw/.cache/afactory/review-target/debug/deps/it-d1168c0a2580cada --exact self_optimizer::light_strategy_generates_one_candidate_without_exposing_source_to_author_workers --nocapture
```

The common-Store test invoked `catalog init` once (0.79 s), `catalog test`
once (3.61 s), six `task plan` calls (4.40–5.58 s each), six first
`task run --execute` calls (14.19–98.14 s), six replay calls
(0.47–1.68 s), and two reviews (21.98 s and 51.75 s). Its trace
also contains 247 Git subprocesses with 16.13 s cumulative nested process
time. The exclusive test's `self optimize` calls took 21.48–33.30 s;
its task run calls took 37.28–96.05 s, and its trace contains 188 Git
subprocesses with 9.27 s cumulative nested process time. Those subprocess
times overlap parent processes and must not be summed into wall time. Some
nonzero CLI exits are asserted negative cases; both test processes exited zero.
Process-only tracing does not isolate Python startup, fsync, worker execution,
or receipt persistence. It gives no basis to shorten sleeps, leases, deadlines,
or process checks.

The previous attempt in an incomplete local registry could not compile the
baseline or candidate: offline Cargo lacked `serde`, and the network attempt
stalled on crates.io DNS. Its metadata failures were not test failures or
performance observations. The supplied external testing report used an older
source and Cargo-runner comparisons on a Mac; it does not establish a current
Linux nextest gain. The present fixture decision follows the captured original
profile and the shared-state coverage requirement: preserve the aggregate
byte-identical to S0 and make no speculative fixture optimization. No
post-change fixture speed comparison exists because the fixture did not change.

## Experiment workflow and manifest proof

`ci-shard-experiment.yml` runs on a same-repository `pull_request` only when
the workflow, helper or this protocol changes. It uses `contents: read`,
checkout without persisted credentials, and no `pull_request_target`. Each
job checks out and records the exact PR head SHA. [GitHub documents the
default-branch requirement for `workflow_dispatch`](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow);
that event is not the premerge trigger. Opening a PR can trigger the first run;
subsequent **Re-run all jobs** actions retain the same head SHA as long as the PR
is not updated.
Do not merge merely to run this experiment.

Each run contains three variants on Linux with the pinned toolchain, nextest,
the repository cache setup and four nextest threads:

| Variant | Build and execution |
|---|---|
| `single` | One `cargo test --locked --no-run`, then the complete default nextest selection. |
| `archive-build` + two `archive-shard` jobs | One checked nextest archive, artifact upload and download, then two count partitions. |
| Two `rebuild-shard` jobs | Each runner builds independently, then executes one count partition. |

Before test execution, each variant records the full, first and second
manifests and verifies nonempty, disjoint, exhaustive runnable selections and
identical ignored inventories. The archive shards verify the checked manifests
again after download. The aggregate compares the manifests from the two
actual rebuilt runners against both full inventories. `--no-tests fail`,
`fail-fast: false`, and the aggregate's four required job results preserve
zero-test rejection and failed or cancelled shard outcomes. Count partitioning
balances test counts, not runtimes. The existing nextest exclusive override
continues to apply inside the shard containing `light_strategy`.

## Matched-run measurement protocol

Run at most 20 workflow attempts on one exact PR head, first aiming for three
complete matched runs and then up to ten if useful. Use sequential **Re-run
all jobs** on that same head, retain run ID/attempt and head-SHA files, and
record whether each attempt started with a cold or warm compile cache.
`af.ci-step/1.commit` may contain GitHub's PR merge SHA; the checked-out
`head-sha.txt`, asserted in every job, identifies the measured source. An
attempt is comparable only when all build arms have the same cold/warm
classification and comparable sccache hit rates; retain and label any other
attempt but exclude it from the matched median. Concurrent cache writes may
prevent comparability even when the starting classification agrees. Capture the
GitHub jobs and steps API `started_at`, `completed_at`, and conclusion for
every job/step; retain `ci-step.py` JSONL, JUnit, manifests, CPU/toolchain
files and sccache statistics. In particular, the API measures the `Share
checked archive` upload and each `Download checked archive` step, which
`ci-step.py` cannot wrap because they are GitHub Actions. Note cache-hit-rate
differences between concurrently running jobs; they remain a confound.

Report both observed and adjusted results for **each** run. The observed
single latency spans workflow creation to the `test` step finish. Archive
latency spans workflow creation through archive build, checked-archive upload,
and the later of the two shard `test` finishes. Rebuild latency spans workflow
creation to the later rebuild shard `test` finish. These spans include queue,
install and transfer delays. Sum the corresponding job active durations
through test completion as a runner-work proxy; do not call this billed runner
minutes. Record checked-archive upload and both downloads as separate API
step durations and keep them **included** in archive operating cost. Exclude
retention uploads and the aggregate job from all three operating comparisons.

All `record`/`verify` invocations have the `manifest-proof` label in
`ci-step.py`, including the unsharded arm's three listings and verification.
This proof is an experiment-only control. For an adjusted comparison, subtract
its measured duration from each job's operating span. Also subtract the API
step durations for `Record runner and exact head` or `Record exact head`,
`Record compile cache statistics`, and `Retain build measurements` wherever
those steps precede the relevant test finish. The latter two run before the
archive shards can start because they finish the required `archive-build` job.
Then recompute the maximum shard path and total runner-work proxy. Show the
raw and adjusted numbers together; do not subtract checked-archive upload or
download, extraction during test execution, builds, or failures. API
timestamps retain step gaps and queue time separately, so a result must state
whether it uses observed or adjusted latency. Keep every failure and
cancellation in the run table;
calculate medians and spread only over complete successful matched runs,
reporting how many were excluded and why. Do not report P90 from a few runs.

Stage 2 has not run. The earlier reviewer Demands for paired four-CPU GitHub
measurements and for focused local two-CPU nextest timings remain open. Since
the fixture split was abandoned, its claimed benefit has no acceptance basis.
Only a later, separately reviewed change backed by controlled paired evidence
may alter `validate.yml`. Full exact-head GitHub CI, including lint, container,
macOS release and signing gates, is a separate publication requirement.

## Disposition of prior review findings

| Prior finding | Disposition |
|---|---|
| Workflow dispatch could not run before merge | Repaired with a read-only, path-limited `pull_request` trigger and exact head checkout; dispatch retained for after merge. |
| Split starter test lost common Store coverage | Rejected the split; original aggregate remains byte-identical to S0. |
| Fixture edited without profiling | Captured original-source process profile above was inspected before the decision; no fixture edit is made. Controlled speedup evidence remains open. |
| Manifest proof and archive transfer skewed comparison | All proof steps are timed, including single; transfer durations come from the GitHub steps API. Raw and proof-adjusted accounting are defined above. |

The prior Opus report and its Demands remain predecessor evidence, not a clean
Review or closed Demand. Independent full S0-to-final review and native
acceptance checks still follow this implementation.

## Local checks on this repair

`cargo fmt --all -- --check` passed. `python3 scripts/test-task-preflight.py`
passed 21 checks. Python YAML parsing confirmed the read-only trigger, exact
head checkout and four timed proof calls in each build arm. The manifest helper
passed mock cases for a valid partition, overlap, missing tests, empty shard,
ignored-inventory mismatch and partition command construction. A separate
offline miniature Rust crate with three runnable tests and one ignored test
passed full/partition proof both from live binaries and from a nextest archive.
These checks reported zero failures.
These plumbing checks do not constitute repository test acceptance or a
performance benchmark; the native focused Rust checks and independent review
remain pending.
