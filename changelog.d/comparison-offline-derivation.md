- `executor comparison-verify` now derives a native comparison's report again from the bundle
  alone (the declaration's members, each original's state bound to its sealed result, the
  aggregate recomputed from its partitions, run and verdict) and refuses a report that claims
  anything else. A failed telemetry original or queued members relabelled complete, with
  report and manifest rehashed, are refused; honest incomplete and running bundles still verify
  as what they are, and a complete comparison keeps its failing verdict. The
  `af.perf-comparison-report/1` contract refuses a complete lane over an original that is not
  complete, and a complete comparison over one that is not verified
  ([ADR-0240](docs/adr/0240-execute-performance-experiments-independently-on-a-locked-vm.md)).
- A CPU capture whose recordings never hit the profiled executable's known build ID is omitted
  as `executable_not_sampled` and nothing is rendered from it; `profiles.json` refuses such a
  capture without that omission. A build-ID-less (musl) executable stays bound by its exact hash
  and symbol archive.
- The local executor guide gives the full release comparison's configuration (the whole
  nightly family on the musl target in 4 shards, the 12 manual telemetry runs, a fresh
  candidate journal) and the checks of its frozen plan and telemetry workload before it runs.
