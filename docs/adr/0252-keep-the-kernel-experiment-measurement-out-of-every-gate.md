# ADR-0252: Keep the kernel experiment measurement out of every gate

**Status:** proposed (2026-10-27). Amends the test scheduling described by
[ADR-0185](0185-measure-guarded-kernel-experiments-through-a-literal-bootstrap-over-a-protected-harness.md)
and [ADR-0186](0186-fold-protected-paths-key-warm-targets-by-the-whole-snapshot-bound-measure-output-and-replan-bridge-runs.md).

## Context

The guarded kernel integration test combined tampered-harness refusals with a judged comparison.
The comparison runs a nested `af task start --execute` and builds `perf-probe` in release mode.
On 26 October 2026 it took 1,147 seconds locally out of 1,431 seconds of exclusive time in a
3,640-test suite. On 27 October CI reported it as `SLOW [>960.000s]` in a 3,178-second test
step. Nextest reserved every test thread for this test, leaving the other threads idle.

## Considered options

1. Keep the combined exclusive test in every gate. Rejected because the cold release build
   dominates gate time and blocks parallel tests.
2. Remove the integration test. Rejected because the guarded refusals and judged receipts must
   remain tested.
3. Raise timing budgets or reduce repetitions. Rejected because that would change the measurement
   contract rather than its scheduling.

## Decision

Split the integration into an ordinary perf-class test in which every tampered harness is
refused by the kernel as a baseline, with no build, and an ignored `manual-measurement` test
for the judged comparison plus tampered-candidate refusals after a measured baseline. Run the
latter with
`make perf-kernel-measure` before a pull request changing `scripts/performance/`,
`crates/review-pipeline/examples/perf-probe/`, `crates/review-pipeline/build.rs`, the experiment
runtime or kernel receipts. The build-free refusal test does not need suite exclusivity. Preserve
the existing assertions, measurement budgets, deadlines and repetition count.

## Consequences

The measurement's own regression is caught by the manual run and by Performance workflows that
measure releases, not by a pull request gate. The build-free baseline refusal paths remain in the
perf gate; the measured candidate refusals run manually. The gate pull request and
`make check-full` are expected to be about 16 to 19 minutes shorter, based on
the observed runtime of the former combined test; this is an estimate, not a new measured result.
No persisted or printed contract changes.
