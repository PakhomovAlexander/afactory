- The performance laboratory's `compare` now refuses evidence it used to pass: a missing advisory
  or sentinel record, a shard whose terminal record says failed or interrupted, records out of
  planned order, and builds whose recorded recipe settings or Cargo.lock differ from the frozen
  recipe and the digests `plan` now reads from each source's Git objects. A missed enabled budget
  fails a complete healthy case even when its interval is inconclusive. The plan, build and
  verdict contracts are now `af.perf-run/2`, `af.perf-builds/2` and `af.perf-verdict/2`, and their
  `/1` documents are refused. The required `lint` check now fails unless the laboratory's Python
  3.9 and 3.11+ legs succeed
  ([ADR-0162](docs/adr/0162-freeze-performance-plans-and-judge-them-by-exact-paired-verdicts.md)).
