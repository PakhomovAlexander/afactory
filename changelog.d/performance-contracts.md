- The performance laboratory's first package: a reviewed registry of the design's initial cases
  (`fixtures/performance/suite.toml`), closed versioned evidence contracts with generated
  schemas and golden fixtures, and `python3 scripts/performance/cli.py` `list`, `plan` and
  `compare`. `plan` resolves baselines and the measuring authority and freezes the seeded paired
  schedule; `compare` recomputes an exact `paired_median_tolerance_v2` verdict from retained
  evidence and refuses missing, duplicate or tampered records. Nothing executes a workload yet,
  and full plans stay unavailable until a calibrated activation exists
  ([ADR-0162](docs/adr/0162-freeze-performance-plans-and-judge-them-by-exact-paired-verdicts.md)).
