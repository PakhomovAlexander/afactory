- The performance laboratory's native probe: `crates/review-pipeline/examples/perf-probe.rs`
  times each library case's real production operation (canonical serialize and validate, CAS raw
  and typed reads, Git source capture of 100 and 10,000 files, cold and prewarmed projection of
  exactly 10,000 Task events, the current-plan check and first `TaskRuntime::new` at both issue
  #255 package sizes, and an advisory persistence round trip) on fixtures it generates through the
  product's own writers, with setup timed apart. Independent golden entries
  (`fixtures/performance/golden/native.json`) come from a reference generator that never runs the
  product, and `python3 scripts/performance/cli.py probe` and `judge` check each side against
  them. The registry is now `af.perf-suite/2`; its `/1` is refused. Nothing runs paired
  comparisons yet
  ([ADR-0163](docs/adr/0163-measure-native-workloads-through-a-public-api-probe-and-independent-golden-oracles.md)).
