Reduce repeated performance-fixture setup by writing admitted authority Tasks once per side
and shard, then restoring a private copy with a cold validator and a fresh production lease
for every sample. Distribute the complete quick suite over sixteen schedules with the existing
two-job concurrency, pair counts, coverage and wall-time bounds. Require complete full-suite
bootstrap evidence before calibration; enforcement remains advisory.

Authority compatibility: committed `.af/` policy is unchanged. The performance authority,
authority probe contracts and staged protected-harness digest change; earlier calibration
evidence does not activate this harness.

Seed readers construct the cold production validator with a non-executing host; the full
execution host is captured by the seed writer. This removes unused per-sample setup while
keeping the timed Store checks and runtime construction unchanged. Reader contracts advance
to version 3, and earlier contracts are refused.

Make diagnostic seed-reuse and flamegraph tests request exactly three fresh executions, so
FIFO scheduling delays cannot change the exercised repetition count. Production capture
bounds and the separate recording-cap tests are unchanged.
