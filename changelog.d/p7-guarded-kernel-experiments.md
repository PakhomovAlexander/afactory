- Guarded kernel performance experiments. The new `cli.py measure --case ID --metric KEY` is
  one repetition of a kernel `measure`: it builds the native probe offline into the kernel's
  Cargo target, runs one golden-judged nonspawning library sample and prints one
  `af.measure-report/1` line, or exits nonzero with no report. Before importing the driver or
  building anything, the literal isolated bootstrap of the staged policy verifies the whole
  protected harness against the digest the captured policy holds. The harness is the driver,
  fixtures, probe, adapters, trace and calibration helpers, and every manifest, lock, build
  script, toolchain pin and Cargo configuration, with additions. The bootstrap refuses links and
  stray bytecode. `fixtures/performance/kernel-measure/` stages the measures for a reviewed
  human installation; `cli.py harness-digest --check` runs in `make check` and release
  preparation, `--write` re-pins. Objectives compare `elapsed_ms` until the guard is accepted. A
  real kernel integration proves tampered candidates never reach an improved receipt. A warm
  target never measures another revision's product. Costs count every retained execution, fail
  any with a failed sample, timeout or unhealthy sentinel, and observe real cold or primed warm
  compiled caches. Shipping keeps one target per binary and host group, with median and p95, and
  never demonstrates from unhealthy runs. A bridge's protected diff covers the instrument helpers,
  and fetched bridge runs are bound to the Performance workflow, attempt and hosted jobs
  (`af.perf-builds/6`, `af.perf-cost/3`, `af.perf-shipping/3`,
  `af.perf-activation-verification/3`, `af.perf-kernel-inventory/1`;
  [ADR-0185](docs/adr/0185-measure-guarded-kernel-experiments-through-a-literal-bootstrap-over-a-protected-harness.md)).
