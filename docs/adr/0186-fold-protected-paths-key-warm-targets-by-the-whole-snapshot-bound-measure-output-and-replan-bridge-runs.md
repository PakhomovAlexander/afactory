# ADR-0186: Fold protected paths, key warm targets by the whole Snapshot, bound measure output and replan bridge runs

**Status:** accepted (2026-10-21). Implements package p8 (integrated acceptance) of the
[continuous performance laboratory](../design/continuous-performance.md) (design §13, §15, §17)
on the published p7 source of
[ADR-0185](0185-measure-guarded-kernel-experiments-through-a-literal-bootstrap-over-a-protected-harness.md).
It repairs the four confirmed p7 findings, the evidence and scheduling gaps of p7's native
integration, the report's first view and wording, and the diagnosis of p7's failed gate. The
accepted design is unchanged. ADR-0162 to ADR-0185 stand except where this ADR amends them.
[`docs/performance-acceptance.md`](../performance-acceptance.md) is the acceptance matrix: what is
implemented and tested, what was measured, which environment is unavailable and what stays
pending.

| Contract | Version | What changed |
| --- | --- | --- |
| Kernel receipts | `af.perf-kernel-receipts/2` | new published contract: every Task's result, each output's typed envelope and every repetition's bounded public streams; the p7 compact `/1` is refused |
| Kernel sources key | `af.perf-kernel-sources/2` | the content digest a bound target remembers now covers the whole measured Snapshot; a `/1` key never matches, so the target is invalidated once |
| Report | `af.perf-report/2` | unchanged shape; the page leads with the verdict, and the wording follows the evidence kind |
| Kernel inventory | `af.perf-kernel-inventory/1` | unchanged shape; re-pinned for the changed harness |

## Context

The p7 Task's mandatory gate failed (receipt
`sha256:889bf9e322fef0f941c0ac415addb6e1aad23cbdedcd72ee72c0a7e15adc3fac`), so both semantic
reviewers and the goal evaluator were suppressed; empty findings are not a pass. Independent
validation on p7's sealed source confirmed four findings:

1. **Case-variant build inputs** (`p7-case-variant-build-inputs`). The guard classified protected
   paths case-sensitively. On a case-insensitive filesystem, Cargo runs `BUILD.RS` as a build
   script and reads `.CaRgO/config.toml`, yet both kept the guard's digest and the bootstrap
   imported the candidate driver.
2. **An incomplete source key** (`p7-cache-source-key-incomplete`). The digest that decides
   whether a warm target is stale skipped every directory named `target`, including
   `crates/fixture/src/target/lib.rs`, which Cargo compiles. It also missed what packages read
   outside their member directories: schemas, fixtures and brand tokens through `include_str!`,
   and the root `.cargo` configuration.
3. **Unbounded capture** (`p7-kernel-output-unbounded`). `sample` buffered a probe's whole stdout
   and stderr before truncating: 1,114,112 stderr bytes were held, over the 1-MiB hard sample
   maximum. The build log was unbounded and a failure read the whole log to print its tail.
4. **Unverified bridge plans** (`p7-bridge-frozen-plan-not-verified`). Fetched bridge runs were
   bound to their producer but not their plan. A genuine plan frozen from Git, with only the
   post-merge A/A's absolute tolerances widened from 1 to 1000 and its digests recomputed,
   turned a candidate at 30 against a baseline at 10 from a regression into a verified bridge.

p7's native integration passed in 452 s, but its receipts named streams in a Store deleted at
test exit. The committed receipts flattened the result that the exporter emitted in full. The
test asserts a real elapsed-time improvement yet was scheduled beside other tests. The failing
gate test,
`campaign_loop::committed_and_dirty_diff_subjects_execute_the_wired_change_set`, saw `-1` with
empty streams after 0.119 s: its helper mapped a missing exit code to `-1` and discarded the
`ExitStatus`. A root review of the report found that a synthetic report claimed pairs ran on one
host and were measured. It also found the first view buried the regression under a long
eligibility paragraph and a fifteen-column table.

## Considered options

1. **Lower-case only ASCII names.** Rejected: case-insensitive filesystems also fold Unicode
   case and equate canonically equivalent spellings. The guard folds the Unicode default way.
2. **Keep skipping `target` but only at the workspace root.** Rejected: a name still decides,
   and a measured Snapshot never holds a build target, since the kernel's is outside it.
3. **Hash only what Cargo's last dep-info listed.** Rejected: the paths name the previous
   materialization, and the decision must be made before the build. The whole Snapshot is a
   superset. Its hashing time was not measured for this decision; ADR-0187 records one
   size-, host- and repetition-qualified observation and claims nothing for larger Snapshots.
4. **Run the probe in its own session and kill that session.** Rejected: the kernel's check
   runner ends the measure's process group when it exits (`ExitPolicy::KillProcessGroup`). A probe
   in another session would escape that cleanup if the kernel killed the measure at its deadline.
5. **Compare only a bridge plan's policy digest.** Rejected: the policy digest is the registry's,
   and the attack changed the frozen plan, not the registry. The whole frozen identity is
   compared.
6. **Retry or skip the failing gate test.** Rejected: the cause was not established. A clean
   repeat proves nothing about the historical process, so the helper now keeps the real status.

## Decision

1. **Caseless protected paths** (`kernelguard.BOOTSTRAP`). `protected` and the staged subtree's
   refusals compare NFD-normalized, case-folded paths against `FILES`, `NAMES`, `TREES` and
   `.cargo`. `BUILD.RS`, `.CaRgO/config.toml` and `CARGO.TOML` are protected wherever they
   appear, and `cargo.toml` in the staged subtree is refused. The staged exclusion itself stays
   exact, so a case variant of it is protected. Ordinary product sources stay free.
2. **The whole Snapshot keys a warm target** (`kernelmeasure.sources_digest`,
   `af.perf-kernel-sources/2`). Every regular file of the measured Snapshot is hashed by path and
   content, whatever its directory is called, within 200,000 entries and 2 GiB. A link or special
   file is refused. Modification times never decide. A changed key removes only the workspace's
   own release fingerprints; dependency builds stay warm.
3. **Bounded capture while reading** (`kernelmeasure.capture`). The probe's stdout and stderr
   together are bounded by the registry's `sample_log_bytes` (256 KiB) as they are read. Memory
   keeps at most one probe record plus one byte of stdout and a 4-KiB stderr tail. A probe that
   outgrows the bound or its timeout is killed at once and fails with no report. The build's
   merged output is counted, with at most 4 MiB kept in `build.log` and a 4-KiB tail in memory.
   A stream still held 5 s after its process exited fails the step. Probe and build stay in the
   measure's process group, which the kernel ends when the measure exits.
4. **Bridge runs are planned again** (`activation.bridge_replanner`). Before a fetched bridge run
   is recomputed, the bridged authority freezes its plan again from Git as the reviewed
   workflow's manual bootstrap dispatch would. The golden check plans the calibrated against the
   bridged product source and the A/A one source against itself. Only the run ID, control
   revision, shard count and frozen build come from the retained plan. The frozen identity,
   everything but selector text and digest, must be equal. A changed policy, workload, timing,
   environment, recipe, fixture or instrument field is refused. A later control revision with
   byte-identical producer code is admitted.
5. **Receipts keep what they claim** (`af.perf-kernel-receipts/2`,
   `crates/af/tests/it/perf_kernel_measure.rs`). Each Task records its exit code and inspection
   result, and each output its complete typed envelope and subject Snapshot. Each measured
   repetition records stdout and stderr with the kernel's content ID, SHA-256 and size: whole up
   to 64 KiB, else the first and last 32 KiB. The export is checked against the published schema
   and the laboratory's reader before `$AF_PERF_KERNEL_RECEIPTS` receives it. The test also
   asserts that the guard's line precedes the setup record in every measured repetition. It adds
   a sixth tampered candidate, `BUILD.RS` and `.CaRgO/config.toml`, which must be refused before
   any build. The p7 `receipts.json` stays as recorded (`/1`, pre-sealing p7 source); no code
   reads it.
6. **The timed integration runs alone** (`.config/nextest.toml`, ADR-0124). The kernel
   integration joins the exclusive override because its objective compares real elapsed time.
   Timeouts and leases are unchanged.
7. **Exit statuses are kept** (`campaign_loop::invoke_af`, `perf_kernel_measure::Consumer::af`).
   An `af` that ends without an exit code fails at once with its `ExitStatus`, signal,
   core-dump flag and streams, never as a made-up `-1`.
8. **Reports lead with the verdict** (`report.html_bytes`, `report.COMPARISON`). The first section
   gives the result and exit, how many cases are outside tolerance or incomplete, and the scope,
   evidence kind and authority. It lists each eligibility reason as its own item, then tabulates
   the affected cases. Each row shows baseline and candidate medians and the absolute and
   relative change. A compact table of every case follows. The exact intervals, tolerances,
   distributions and budgets, the provenance and the raw sequences sit in expandable details.
   Coverage, health, shards, diagnostics and downloads keep their sections. A synthetic report
   says it holds constructed values that nothing executed. A measured report states only what its
   retained shard records show, and mixed or empty evidence is named as such. Every datum, the
   exact verdict, the CSP and offline operation are unchanged.

## Consequences

- **Tests.** `test_kernelguard` covers caseless classification, two case-variant candidates that
  the real bootstrap refuses before import, and staged case variants. It also covers the source
  key over a `target` directory with preserved modification times and a warm-target
  invalidation from it. It kills a probe that never stops writing and refuses a held stream. It
  bounds the build log and its tail, times out a build, and checks long receipt streams.
  `test_activation` freezes bridge plans from Git as the dispatch does. It refuses the
  widened-tolerance A/A and changed environment, instrument and fixture fields, and admits
  another control revision. `test_report` checks wording by evidence kind and the verdict-first
  layout. `test_contracts` checks the receipts' schema fixtures.
- **Evidence.** No number here is a product speed claim. Three isolated repeats each of the
  failing campaign test and of the `build_metadata` test that p6b's gate labelled LEAK passed
  without a LEAK label. That does not establish the historical cause; the next unchanged gate is
  the evidence.
- **Pending operational evidence.** The acceptance matrix lists it: the five-day hosted cohort,
  proposed activation, a real bridge, the cost and shipping runs, contained Linux captures, the
  staging release handoff, archive collection and ruleset coexistence. Installing the staged
  policy and selecting the inner metric also remain human actions.
- **Not changed.** The 15-second lease, the 8-MiB envelope cap, the sample and build limits of
  design §14, every gate, timeout and budget, and advisory enforcement.

**Amended by [ADR-0252](0252-keep-the-kernel-experiment-measurement-out-of-every-gate.md)
(2026-10-27).** The guarded integration's judged comparison is now an
ignored manual measurement run with `make perf-kernel-measure`; tampered-harness refusals remain
in the perf gate. The measurement and guard contracts above are unchanged.
