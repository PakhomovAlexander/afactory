# Bind calibration to each case's observed shard CPU group

Status: accepted, 2026-10-22. Implements design §15's per-case and observed-group calibration.

## Context

Hosted full-suite runs assign shards to different CPU models. The calibration judge instead
required one CPU group for an entire run. Thus a run whose paired samples all stayed on their
own healthy, allowed hosts was invalid solely because another shard used a different allowed
CPU. Its samples could not prepare parameters, and no such trial could establish readiness.

## Decision

Derive each case's CPU/image group from its planned shard's verified evidence. Preparation
selects A/A samples and measured loop costs by that case's observed group. Dose checking
uses the same binding to select its previously sealed target and iteration count. A pair
never crosses hosts, samples of different groups are never pooled for a case's parameters,
and every shard must still use an allowed, prepared group when applicable.

Readiness advances to `af.perf-readiness/6`: each member has `cpu_groups`, a case-ID map of
observed group IDs, with null for unavailable or unrecognized observations. Earlier readiness
versions are refused. A complete member binds every declared case to an allowed group and
contains every declared case outcome. Unknown and unprepared hosts, missing shards, unhealthy
samples and failed originals still disqualify the entire member. Original-attempt and
producer checks, frozen parameters, doses, pair counts and acceptance thresholds are unchanged.

All 20 A/A trials must still qualify, and at least nine of ten whole power trials must pass
in every critical case. The global tally counts each trial once. Descriptive group tallies
count participation once in every observed critical-case group; these counts may overlap
and cannot be summed as independent trials or used to combine successes across trials.

## Consequences

Mixed-host fixture cohorts use different A0 values and loop costs for two CPU groups, rotate
hosts during preparation, and exercise the real planner, evidence collection and readiness
calculation. They check independent parameters, correct per-case targets, whole-trial counts,
unknown-host refusal and missing or relabelled case-group refusal. A substituted case set
remains an invalid original in the denominator, with absent declared cases unobserved.
Activation recomputes the
same complete readiness document. No real cohort or empirical readiness is claimed by tests.

The calibration authority and protected harness digest change. Full-suite sampling semantics
are unchanged. A future cohort must name the merged revised authority; existing failed runs
remain retained and no enforcement is enabled by this repair.
