# ADR-0185: Measure guarded kernel experiments through a literal bootstrap over a protected harness

**Status:** accepted (2026-10-21). Implements package p7 of the
[continuous performance laboratory](../design/continuous-performance.md) (design §13, §15) on
[ADR-0132](0132-measure-and-compare-source-candidates-in-the-kernel.md)'s `measure` and
`compare` operators, and repairs the eight reconciled findings on
[ADR-0184](0184-verify-proposed-activations-in-trusted-ci-and-bind-bridges-groups-and-costs-to-retained-executions.md)'s
source. The accepted design is unchanged. ADR-0177 to ADR-0184 stand except where this ADR
amends them; ADR-0184's statements that costs count distinct source pairs, that a prefetched
Cargo home is a warm cache, and that shipping pools samples by environment label are
superseded here.

| Contract | Version | What changed |
| --- | --- | --- |
| Builds | `af.perf-builds/6` | `build_cache`: the observed compiled cache, a warm build's priming; `/5` is refused |
| Cost | `af.perf-cost/3` | one execution per retained run, every execution failure, observed compiled cache, priming time; `/2` is refused |
| Shipping | `af.perf-shipping/3` | one target per product identity and environment group, with max, median and nearest-rank p95; `/2` is refused |
| Activation verification | `af.perf-activation-verification/3` | a fetched bridge run's producer, attempt and hosted jobs; `/2` is refused |
| Kernel inventory | `af.perf-kernel-inventory/1` | new: the staged protected-harness inventory |

## Context

Design §13 lets an AF experiment measure with the kernel's own Measurement and Comparison
receipts, but only if a candidate cannot change the instrument that measures it, and until a
real tampered-candidate integration proves that, only the kernel's elapsed time may decide.
Nothing existed: no `measure` adapter, no protected-input digest, no staged policy and no
re-pin lifecycle. The predecessor also left eight defects:

1. **Repeated executions collapsed** (`cost-independent-executions-collapsed`). Five complete
   same-source runs at one SHA with distinct run IDs and bundles counted `completed = 1`.
2. **A dependency prefetch was called a warm build cache** (`cost-prefetch-mislabeled-warm-build`).
   Every build target was fresh; only the Cargo home was prefetched.
3. **The calibration helper was outside the bridge's protected diff** (`bridge-instrument-closure`).
   A bridge changing `perf_calibration.rs`'s loop beside its adapter passed.
4. **Fetched bridge runs had no producer** (`bridge-producer-provenance`). Artifact listings
   alone were fetched; any workflow could upload measured-looking bundles under the lab run ID.
5. **Failed samples counted** (`cost-failed-samples-counted`). A sample timeout, a missing
   required metric or a failed sentinel left a run `complete`.
6. **Unhealthy shipping runs demonstrated** (`shipping-unhealthy-evidence`). A failed sentinel,
   an unstable shard or an unusable build did not keep budgets incomplete.
7. **Shipping pooled product identities** (`shipping-mixed-identities`). Two 12-sample runs of
   different candidate SHAs and binaries, or hosts, reached 20 under one environment label.
8. **Shipping omitted the distribution** (`shipping-distribution-fields`). Only the maximum was
   shown; the design requires maximum, median and p95.

## Considered options

1. **Measure CLI cases or nested containers under the kernel.** Rejected for v1 by the design:
   inner measurements are nonspawning library cases only. Source capture is a library case that
   runs Git, so it is refused too.
2. **Read the expected digest from a file in the measured tree.** Rejected: a candidate would
   control it. The digest and the bootstrap are literal arguments of the captured policy.
3. **Import a verification module of the candidate's driver.** Rejected: that is the code under
   test. The bootstrap uses only the standard library, in isolated mode, before any import.
4. **Protect an inventory of today's files.** Rejected: a new `.cargo/config.toml`, manifest,
   build script or bytecode file would pass. Every name of the build closure anywhere in the
   tree is protected, and every file of the protected trees.
5. **Let warm Cargo targets decide freshness by modification time.** Rejected after the real
   integration measured a reverted candidate with its baseline's sleep: materialized files can
   carry times Cargo takes for unchanged sources. The adapter invalidates by content.
6. **Call any prefetch or reused directory a warm build.** Rejected: only a target this run
   primed from the same sources is warm, and the dependency fetch stays separate.

## Decision

1. **One judged sample per repetition** (`cli.py measure --case ID --metric KEY`,
   `perflab.kernelmeasure`). `CARGO_TARGET_DIR` and `AF_CHECK_SCRATCH` must be absolute and
   outside the read-only measured Snapshot. The probe is built with `cargo build --release
   --locked --offline`; a missing input fails. The target's prior state, the probe before and
   after, the build time and the sources it built are recorded on stderr as
   `kernel-measure setup: {...}`, observed, never inferred. When the workspace sources differ
   from those the target last built, the workspace's own release fingerprints are removed
   first; dependency builds stay warm. One instance of `KERNEL_CASES` (the in-process canonical
   and CAS cases: library, no containment, no seed, one repetition) runs once in a fresh scratch
   and is judged by the golden oracle. Only then is the last stdout line the existing
   `af.measure-report/1`, with exactly the requested metric in its declared unit and canonical
   decimal. Every failure exits nonzero with a diagnostic and no report. There is no inner
   median, comparator or retry; the kernel owns repetitions, `elapsed_ms` and the comparison.
2. **The literal bootstrap** (`kernelguard.BOOTSTRAP`). The staged command is `python3 -I -S -B
   -c BOOTSTRAP DIGEST measure ...`. It removes the current directory from the path, walks the
   whole measured tree and refuses any symbolic link, special file or undecodable name. It
   hashes every file of `scripts/performance`, `fixtures/performance`, the probe and supervisor
   examples, and `perf_calibration.rs` and `perf_trace.rs`. It also hashes every `Cargo.toml`,
   `Cargo.lock`, `build.rs`, `rust-toolchain(.toml)` and file under a `.cargo` directory. A
   missing required file, an extra or changed one, or another digest exits 2 before the driver
   is imported or anything is built. The only exclusion is `fixtures/performance/kernel-measure`,
   which may hold only `.md`, `.toml` and `.json` data. The tooling executes the same text, so
   the rules have one definition.
3. **The staged policy and re-pin** (`fixtures/performance/kernel-measure`, `cli.py
   harness-digest`). `--write` deterministically renders four measures (three repetitions of at
   most 20 minutes, `warm = true`) with their `*_elapsed` objectives, and `inventory.json`. The
   inner `wall_ms` is recorded in every Measurement but stays `pending`: no staged objective
   selects it. `--check` runs in `make check` (`performance-kernel-harness`) and
   `scripts/release.sh`. It fails while the staged files are stale or an installed guarded
   measure in `.af/code-policy.toml` differs. The release commit re-pins the staged files after
   its version bump; an installed measure is re-installed by hand. Workers never edit `.af/`.
4. **Real kernel integration** (`crates/af/tests/it/perf_kernel_measure.rs`). The consumer
   repository holds this workspace's files and the experiment fixture's authority, with the
   staged tables appended verbatim; the repository check must agree. The test runs:
   - an empty warm Cargo home, whose offline build fails;
   - a controlled slowdown in `validate_envelope`, a disposable product revision, which a
     candidate removes with the harness byte-identical: the kernel's comparison is `improved`,
     and the inner metric folds to `improved` too;
   - five tampered candidates that also remove the slowdown: the timing boundary, a golden
     digest, an added `.cargo/config.toml` with a marker-writing build script and an extra
     manifest, a link to a protected file, and an unchecked-hash `.pyc` that writes a marker
     when imported. Each fails at its first repetition with exit 2 and no build. Its comparison
     is `inconclusive`, its evaluator never runs, and no owned marker exists. A linked parent
     directory and an edited driver are refused the same way in `test_kernelguard`;
   - a staged policy the candidate edited, which keeps the captured command;
   - another unit, another key and a refused case: `unit_mismatch`, `malformed_report`, `exit`.

   `$AF_PERF_KERNEL_RECEIPTS` receives the exact public payloads of every Task.
5. **Costs count executions** (`costs.execution_failures`, `cost_groups`). A run fails its
   attempt for any build, shard, record, sample (timeout and missing metric included), health
   (failed sentinel, unstable shard) or integrity failure; `complete` means none. Groups count
   each retained run and bundle once. A healthy regression counts.
6. **Compiled caches are observed** (`builds.Builder._prime`, `--build-cache cold|warm`). Cold
   requires every worktree target observed empty. Warm first runs a bounded priming build of
   each worktree's products (the supervisor, AF) under the run's budgets, records it, and
   requires every target primed before the measured pass. `build_ms` is the measured pass,
   `priming_ms` apart.
7. **Shipping targets are product identities** (`costs.shipping_report`). Samples pool only for
   one candidate SHA, shipping binary, recipe and hardware/image fingerprint. A run's build,
   integrity or shard-health failure leaves its instances incomplete, samples shown. Each target
   has its own denominator and exact max, median and nearest-rank p95, which the reader
   recomputes; a budget instance is met only when every target is.
8. **Bridge closure and producer** (`planning.INSTRUMENT_HELPER_PATHS`,
   `activation.bridge_provenance`). The trace and calibration helpers are harness paths. A
   fetched bridge run needs:
   - its run record, the attempt its lab run ID selects and that attempt's complete jobs;
   - the reviewed `performance.yml` of the cohort's repository, completed successfully;
   - a head-commit workflow and control-revision scheduler byte-identical to the bridged
     authority's;
   - every build and shard job on a fresh hosted runner.

   The fetched verification records this as each run's `provenance`.

## Consequences

- **Tests.** `test_kernelguard` runs the bootstrap over tampered trees with import markers,
  planted standard-library names, the staged render and check, an installed stale measure, and
  the adapter's success, failure, invalidation and canonical-report paths. `test_costs` covers
  five same-source executions at one SHA, prefetched-but-cold builds, timeouts, sentinels and
  unstable shards, unhealthy and mixed-identity shipping and the distribution.
  `test_activation` covers the helper paths and five producer refusals.
- **Evidence.** The integration's receipts are its kernel payloads; no number is a product speed
  claim. The guarded inner metric becomes selectable only after this integration is accepted on
  a sealed source and a reviewed human change installs and selects it.
- **Pending operational evidence.** No hosted cohort, bridge, cost or shipping run, staging
  handoff or ruleset change has run; they remain p8's.
- **Not changed.** The 15-second lease, the 8-MiB envelope cap, ADR-0132's receipts and
  operators, existing gates, budgets and sandboxes, and advisory enforcement.
