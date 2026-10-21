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

The sample captures its host and authority afresh, asserts the validator is cold, takes its
own production 15-second lease after capture, and opens a fresh Store connection for timing.
The measured operations, independent semantic digests, package sizes, envelope cap and
failure classification stay unchanged. The shared measuring implementation also remains
exercised by direct fixture-writer tests. Changed setup contracts are
`authority_plan_check_v2` and `authority_runtime_new_v2`; the seed writer is
`authority_seed_v1`. Earlier reader contracts are refused.

The quick tier distributes its complete instance set over sixteen shards, with the original
maximum of two concurrent jobs. Each shard still has 15 sampling minutes and the whole run
60 minutes. Pair counts, warmups, sentinels, tolerances, coverage and containment remain
unchanged. Shards still contain whole instances with their paired same-host samples; a pair
never crosses hosts. In the current registry each authority workload shares only with a short
CLI discovery case; state-projection and stateful CLI setup are on other shards. The existing
explicit shard override remains available for experiments. All setup, containment and cleanup
reserves remain enforced, including the refusal to start a sample whose worst case cannot fit.

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
