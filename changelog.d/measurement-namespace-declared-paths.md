- Repair measurement namespace construction for runtime-overlapping private ancestry, relative
  and joined-option inputs, symlink-aware parent traversal and explicit persistent reports. Keep
  temporary directory containers on their original filesystem and remove launch-owned backing
  state after teardown. Extend required non-root AF inventory, planning, release-smoke and actual
  script/executor regressions without changing measured argv, authority checks or wake delivery
  semantics. See the [decision](../docs/adr/2026-11-16-declare-measurement-paths-and-preserve-their-filesystems.md).
