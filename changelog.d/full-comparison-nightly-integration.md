- The independent performance executor carries both main's nightly originals and the durable
  release comparison: new originals are `af.perf-executor-request/3` (main's nightly submission
  plus a profile's parent and a comparison partition), request/1 and main's request/2 nightly
  originals stay readable, and a request/3 shape under an earlier version is refused. The
  configuration is `af.perf-executor/4`: `[core.windows]` declares a core plan's sampling and
  run minutes, frozen into the configuration and plan digests under the unchanged hard maxima.
  The full release comparison recipe declares 45 and 120 minutes over 8 serial shards in 4
  partitions; the ordinary nightly keeps its tier's 15 and 180. The partition ADR is renumbered
  [ADR-0269](docs/adr/0269-bind-comparison-partition-execution-to-reused-build-artifacts.md).
