# ADR-2026-11-10-separate-long-running-e2e-performance-cases: Separate long-running E2E Performance Cases

**Status:** proposed, 10 November 2026.

## Context

Long development flows need a separate execution and calibration policy. The supplied reference
implementation's completed local records and inconclusive A/A verdict describe reference behavior
only. They are not measurements of the current-main source port. The ordinary 12-pair minimum
and 60-member cohort remain unchanged; a smaller separate class requires explicit review.

The [source-port design](../design/long-running-development-flow.md) records the implemented
manual/calibration path, current native contracts and post-delivery operator checks.

## Considered options

- Force long-running cases through the ordinary schedule: rejected because the measured workload
  cannot fit and dropping records would make the result incomplete.
- Stop early when a result looks favorable: rejected because data-dependent stopping invalidates
  the fixed-N interval and can hide regressions.
- Admit a separate fixed-small-N class with its own schedule and readiness cohort: chosen for
  review. This is a new policy, not an exception written into an active run.

## Decision

1. **Separate class.** A Performance Case may declare `execution_class = "long_running"`. Its
   instances use a dedicated tier and verdict identity,
   `paired_median_tolerance_long_running_v1`, with fixed sample counts and no adaptive stopping.
   Microbenchmark tiers and verdicts are unchanged.
2. **Small exact intervals.** The minimum is eight pairs for a two-instance family and six for
   one. For two instances, `2 / 2^8 = 0.0078125 <= 0.05 / 2`, so eight pairs support a two-sided
   95% sign-test interval. Wider intervals and more inconclusive results are reported, never
   hidden. Missing, substituted or failed records make the affected evidence incomplete.
3. **Feasibility before tier selection.** The long-running tier is not accepted until the runner's
   actual worst-case calculation fits its sampling and total deadlines, including Fixture Setup,
   seed publication, opening and closing Sentinels, preflight, contained-launch overhead and
   cleanup. The illustrative 40-slot cold/primed schedule is not feasible: at 345 seconds per
   slot it needs 230 minutes before the other costs, exceeding the proposed 180-minute total.
   The class therefore remains unactivated until feasibility is established. No
   timeout, minimum pair count, lifecycle bound or required record may be relaxed to force a fit.
4. **Readiness cohort.** The separate cohort is predeclared as four A/A, four injected-slowdown
   and four power originals on one named Linux container CPU/image group. Every original runs the
   complete workload and all correctness/cache oracles.
   - Every A/A original must be complete, healthy and within tolerance for every critical case.
   - Two slowdown originals use 5% injected work and two use 10%. The measured dose must be
     within 25% of the target. All injection is real compilation inside the timed region, never
     sleep, wrapper delay or a fabricated metric.
   - Four power originals use `p = max(10%, 2*e)` for each case/group. At least three must have
     valid injection and a regression verdict for every critical case. Missing, invalid,
     unhealthy or inconclusive originals count as non-detections. This finite 3/4 requirement is
     not a claim of a population detection rate.
   - Preparation first freezes the A/A baseline `A0`, environment group and deterministic work
     parameters. No dose, environment or member may change after results arrive.
5. **Activation is separate and pending.** A valid readiness record, a feasible frozen schedule
   and owner review are all required before this class may execute as an active comparison.
   ADR-0162 and ADR-0181 remain the policy for ordinary cases until this proposal is accepted and
   implemented. The long-running class itself is not yet accepted or activated.

## Consequences

- A long-running case has a bounded path to a calibrated verdict with fewer samples, without
  changing the ordinary minimums or inventing an exception in a run.
- The smaller cohort limits the confidence claimed about sensitivity. It does not prove the
  ordinary cohort's detection target and is never described as equivalent evidence.
- A candidate can fail or remain inconclusive without weakening lifecycle, cache, schema or
  evidence requirements. Reducing the workload or required records is never a valid optimization.
- The source port includes registry, planner, verdict, cohort, report and schema parity changes.
  Exploratory direct runs remain labeled as such and are not an activation.
