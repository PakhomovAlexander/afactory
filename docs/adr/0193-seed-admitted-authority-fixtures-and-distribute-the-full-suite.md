# Seed admitted authority fixtures and distribute the full suite

Status: accepted, 2026-10-21.

## Context

Hosted bootstrap run `39637277881` recorded 134 of 1,592 planned records before its two
15-minute shard budgets stopped further sampling. None of those 134 records failed. The
closing sentinels and remaining cases were unstarted, so this is incomplete evidence.

A representative large authority sample spent 19.802 seconds in untimed fixture setup and
2.932 seconds in its region, within 24.353 seconds of supervised execution. Recreating the
packages, compiling the graph, proposing and admitting the Task for every sample dominates
the cost. The measured operations are the first current-plan check and runtime construction,
with a cold validator and Store projection. Setup writes are already excluded from them.

The seeded eight-shard trial `39659018121` completed every large authority sample with no
sample failure, but still could not start later workloads within the conservative reserves.
Shard 0 spent 633.565 seconds on 98 records before a CLI sample's 255-second worst case
plus the terminal reserve no longer fit. Shard 2 spent 415.966 seconds on 182 records before
the first state-projection sample's 565-second worst case no longer fit. These are retained
incomplete runs, not successful full comparisons. The long authority workloads must not
share a schedule with those later setup costs.

## Decision

The two authority cases read one admitted Task seed per package size, side and shard, through
the existing bounded, supervised, golden-checked seed path. Each side's own public writers
capture its packages, compile its graph, propose and admit its Task. The proposal and
admission each hold their own production lease and release it while live. Only after the
writer process exits may the driver seal its seed. No SQL fixture or candidate-learned golden
is introduced.

Each sample reads a private copy. Restoration verifies the exact six-event prefix, including
both releases, and restores packages against the fixture's trusted file digests. A missing,
damaged, unreleased or expired Task seed fails; an expired setup lease is never replaced to
heal a failed writer. The original revision and its one-hour deadline are preserved. A seed is
scoped to a shard whose longest sampling budget is 45 minutes, never reused in another run.

The sixteen-shard trial `39663723586` still exhausted shard 0 on an AMD EPYC 7763:
32 of its 44 large plan-check records took 561.15 seconds of supervised execution, with a
median of 17.52 seconds per sample. Median setup was 10.292 seconds and the region 4.700
seconds. It completed 1,766 of 1,872 records across the run, with 106 unstarted and no
recorded sample failure. Medium canonical validation was also statistically inconclusive.
Increasing shard count cannot remove that per-case cost.

Each seeded sample constructs a fresh real `CapturedTaskAuthority` and its cold production
validator over the restored compiler. It uses the probe's non-executing domain host: neither
current-plan checking nor runtime construction calls a worker or prepares its context. The
host refuses those capabilities if the operation changes. Full execution-host capture remains
in the public seed writer and direct writer tests; repeating its compilation, package parsing
and worker transport capture in every reader would be untimed work with no consumer.

The sample asserts the validator is cold, takes its own production 15-second lease after
validator construction, and opens a fresh Store connection for timing.
The measured operations, independent semantic digests, package sizes, envelope cap and
failure classification stay unchanged. The shared measuring implementation also remains
exercised by direct fixture-writer tests. Changed setup contracts are
`authority_plan_check_v3` and `authority_runtime_new_v3`; the seed writer is
`authority_seed_v1`. Earlier reader contracts are refused.

The v3 trial `39672809416` still exhausted the two large authority schedules with zero
sample failures. The plan-check shard on AMD EPYC 7763 completed 35 of 44 authority records
at a median supervised time of 16.091 seconds. The runtime shard on AMD EPYC 9V74 completed
39 of 44 at 14.620 seconds. Both stopped at the unchanged 255-second worst-case reservation
for the next sample, leaving their later CLI and closing sentinel work unstarted. These
original attempts remain retained; their incomplete coverage cannot establish readiness.

The quick tier distributes its complete instance set over sixteen shards, with four concurrent
jobs and up to 25 sampling minutes per shard inside the unchanged 60-minute total limit. These
are resource defaults within the existing admitted ranges: the hard maxima remain eight jobs,
30 sampling minutes per quick shard, and 120 total minutes. No hard maximum is raised. Four
jobs allow the long workloads to overlap while later shards still fit the run's 60 minutes;
the longer per-shard allocation accommodates their actual work and the existing reserves.

Pair counts, warmups, sentinels, tolerances, per-sample timeouts, coverage and containment are
unchanged. Shards contain whole instances with their paired same-host samples; a pair never
crosses hosts. In the current registry each authority workload shares only with a CLI discovery
case; state-projection and stateful CLI setup are on other shards. The explicit shard override
remains available for experiments. All setup, containment and cleanup reserves remain enforced,
including the refusal to start a sample whose worst case cannot fit. Resource allocation does
not change a performance tolerance or turn a failed sample into a successful one.

## Consequences

Traces must distinguish the one-time writer, per-sample restoration and the timed operation.
CPU profiles record only the bracketed operation and do not enter performance verdicts.
Operator Linux ARM64 diagnostics cannot establish hosted x86_64 performance or calibration.

Tests check the new contracts and golden coverage, shared immutable seeds across independent
processes, damaged private copies, unreleased setup refusal, original-history preservation,
and fresh leases after capture. The existing lease-expiry tests exercise seeded readers.

A complete hosted bootstrap run of the revised authority must demonstrate the whole schedule
before declaring calibration. Incomplete prior runs stay in the archive. The measuring
authority and protected harness digest change, so this work does not activate enforcement or
reuse earlier readiness claims.
