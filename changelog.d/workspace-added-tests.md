- Complete passing workspace fixture benchmarks with stable additional head tests now succeed
  with explicit non-comparable evidence and no timing comparison. Equal workloads retain timing
  comparisons; missing samples, receipt failures, count drift and test loss remain refused.
  Benchmark evidence is version 2; earlier reports are refused under
  [ADR-0145](../docs/adr/0145-read-only-what-this-release-writes-and-version-every-changed-shape.md).
- Incorporate the exact merged main #1100 report correction: dated ADR collisions use complete
  identities and safe bounded warnings without obsolete renumbering or index instructions.
