# Release validation and performance

RC3 took 94m 48s from its first feature CI invocation to publication. Its successful
feature PR CI took 18m 15s, release PR CI 18m 39s, and tagged workflow 28m 34s.
The remaining time included two timing-fixture failures, fixes and transitions.
Tagged macOS validation took 21m 49s, including roughly 15 minutes executing tests.
The longest release build took 5m 56s; signing and publication took 18 seconds.
Sources: GitHub Actions runs 35225287791, 35226304325, 35228439326,
35230225047 and 35232342160 in PakhomovAlexander/afactory.

## Scheduling and acceptance

```text
PR ------------------------------------> shared validation (Linux + containers)

main/tag -> resolve -> shared validation ----------------------+
                 |    Linux + containers; macOS for release    |
                 |                                            v
                 +--> three release builds + smoke checks ----> sign + publish
                      (only when resolve selected a release)
```

`ci.yml` handles PRs. `release.yml` owns main and tag validation, including ordinary
main pushes where no release is selected. Both call `validate.yml`; the same release
commit no longer receives a duplicate main CI gate. Build jobs overlap validation,
but publication requires successful validation **and** all three builds. No check,
container probe, per-archive smoke or signature check is removed. The workflow runs
are grouped by commit so an earlier main check does not queue a later release.
Only a main commit whose workspace version changes against its first parent may
create the tag. Later commits cannot steal that release even if scheduled first.
Tag pushes never overwrite: a competing tag is read back and its commit checked.
An existing tag on the same commit supports retry; a different commit selects normal
main validation. Read/push failures without an existing tag fail closed.

PR required-check contexts are now `validation / check (ubuntu-latest)` and
`validation / container-probes`; update any rules that name the old `check` and
`container-probes` contexts during rollout. No branch protection or ruleset was
configured on this repository when checked for this change. Main status is reported
by Release, which the README badge now follows.

`make check` remains the portable Cargo gate, with four test threads by default
(`TEST_THREADS` overrides that local bound). Native-provider fixtures spawn multiple
processes per test; host CPU count alone is not a suitable concurrency bound.
CI retains Cargo as its default. The opt-in `make check TEST_RUNNER=nextest` runs:
formatting, Clippy, all ordinary unit/integration tests, separate Cargo doctests,
and the `release-check` resolution tests. The pinned, checksum-verified nextest binary
uses four test slots across binaries, no automatic retries and no fail-fast hiding
later results. Real-time optimizer integration tests reserve all slots. Container
probes remain a separate required Linux job and execute the existing ignored tests.

Latency comparator acceptance/rejection/dispersion tests use fixed measurements.
One end-to-end fixture still measures a real slow control and prepared cache; this
is plumbing evidence, not a real-world savings claim. The missing-toolchain arm
removes the artificial delay because its required result is inconclusive regardless
of speed. The reviewer completion-order fixture already uses explicit coordination.

## Compilation and caches

The check profile uses debug line tables and disables Cargo incrementals, consistently
on Linux and macOS. Cargo caches include platform, architecture, toolchain, profile,
release target/tool identity, dependency manifests and source revision. A compatible
prefix restores prior artifacts; each changed revision can save its rebuilt workspace
instead of repeatedly restoring an immutable stale cache. Failed tests also retain
compiled artifacts. Keys are not verdicts: all required checks always execute.
Only Cargo registry/Git dependencies and target artifacts are cached, never provider
credentials or signing keys. PR caches remain scoped by GitHub; release jobs cannot
promote PR-scoped cache entries into trusted main caches. Cache volume and save/restore
cost must be measured; retaining every revision can increase eviction pressure.

AF's build script resolves HEAD and branch-ref paths through Git, including linked
worktrees and packed refs. It does not watch a permanently nonexistent `.git/HEAD`,
and branch advances invalidate the embedded commit even when HEAD's text is unchanged.

## Measurements

Each gate command records `af.ci-step/1` JSONL with its source commit, run/attempt,
platform, elapsed time and exit code. `test-build` measures compilation of ordinary
test targets. The Cargo `test` record includes test execution **and doctest compilation**;
Cargo cannot prebuild doctests with `--no-run`. The opt-in nextest path records ordinary
test execution separately, while its `doctests` record includes compilation and execution.
Do not treat either Cargo `test` or `doctests` as pure execution time.
Cache records distinguish exact reuse, compatible fallback and misses. Validation
uploads these records even after failures; nextest runs additionally produce JUnit output. Builds upload
their timings separately; only `af-*` binary archives enter release publication.

Retain each failed/superseded run and retry attempt separately. These records
provide measurement inputs for subsequent self-optimizer work; they do not constitute
an authenticated optimization experiment or automatic adoption approval.

Overlapping RC3's observed builds with its checks would have removed approximately
six minutes of critical-path time, assuming available runners. That is a scheduling
estimate, not a measured post-change release. Keep future before/after measurements
separate from this estimate and record cold/warm cache state and runner type.

## Nextest rollout decision

The local experiment found native Codex fixture probe timeouts under cross-binary
scheduling. An earlier standard Cargo run also exposed a transient unavailable
provider, so this is not established as a nextest-specific defect. Keep the pinned
nextest installer/profile and opt-in gate for further investigation; do not change
the required CI runner until a complete comparison passes on Linux and macOS.
No additional test is ignored, and no retry or larger production timeout masks these failures.
The default Cargo gate uses four threads, separate build timing, and all prior checks.

To reproduce the opt-in runner experiment with the pinned binary:

```sh
scripts/install-nextest.sh
PATH="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/af-ci-tools:$PATH" make check TEST_RUNNER=nextest
```

## Local verification record (2026-09-17)

The final default `make check` passed with Rust 1.88.0, debug level 1, incremental
compilation disabled and four test threads: formatting, Clippy and all tests/doctests.
The test command took 912.390 seconds; its separate build step took 14.012 seconds
using the existing local cache.

The nextest experiment ran 1,279 tests in 484.403 seconds (553.063 seconds including
command startup/enumeration): 1,276 passed, three native-provider probes timed out,
and one test was reported as leaky. All three probe tests subsequently passed under
Cargo. These are diagnostic local runs, with background machine activity and no
controlled repeated performance trial; the failed nextest result is not a savings
claim. Earlier interrupted runs, including a disk-full failure, are retained separately.

Workflow syntax, required publication dependencies, immutable checkout identity,
cache failure-path conditions, installer checksum verification, Git metadata regression
and timing-accounting checks passed locally. GitHub run 35252791781 passed Linux
validation, CLI smoke and live container probes: 17m54s workflow wall time and 20m59s
summed job intervals, with cold cache misses and successful cache saves. No post-change
release has been published.

The single Fable 5.1/high AF review took 6m45s and reported one major and three minor
findings. The release tag race is addressed by version-change eligibility and checked
tag races, with executable local-Git regression tests in `make check`. Timing semantics,
the main badge, and status-context migration are corrected above. The historical AF
verdict remains finding-bearing; these are post-review repairs, not a second review.
The initial AF gate was interrupted when its temporary build directory disappeared;
the restarted gate passed before the reviewer ran.

Two measurement demands remain future work: record cache outcomes, archive sizes and
main-cache eviction over 20 subsequent PR/main runs; compare the first two tagged
releases (cold/warm) against RC3 using `gh run view` timings and runner types. One cold PR run
cannot establish either cache retention or release speedup. If eviction dominates,
revise cache retention using those measurements. These demands are not marked satisfied.
