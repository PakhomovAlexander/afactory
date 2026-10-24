- Bring Telemetry into the performance laboratory without touching the timing verdict, the
  frozen `performance.yml` or its calibration cohort. `cli.py profile --telemetry spans|content`
  records each eligible CLI case per side and reads it back through that side's own
  `af telemetry` documents and their committed schemas. The child page and report show phase
  rows, a timeline and a Perfetto file, with partial evidence named and never complete
  (`af.perf-profiles/5`, `af.perf-report/3`). The advisory `Performance telemetry` workflow and
  `cli.py telemetry-run|verify|history` measure Telemetry's overhead, invocation cost and read
  latency on a baseline and a candidate binary, judged by each script's own rule and compared
  only per level on one host. `cli.py probe --trace-output` gives the `authority_runtime_new`
  investigation a phase breakdown
  ([ADR-0199](docs/adr/0199-integrate-telemetry-into-the-performance-lab-as-diagnostics-and-advisory-experiments.md)).
