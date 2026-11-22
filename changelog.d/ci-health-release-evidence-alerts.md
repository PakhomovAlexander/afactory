- CI health diagnoses and their evidence bind to the selected failed job's exact head, including
  release PR heads that differ from main. Captured Task Selection Requirements use that revision;
  main is refused for a different release occurrence.
- CI health findings immediately report every observed failure with its identity, platform,
  conclusion, head, evidence references and release-blocking status. Explicit release alerts are
  sent before diagnosis even when that failure is unselected or diagnosis capacity is exhausted.
  Notifications use version 2 and bounded batches; version 1 fails closed under
  [ADR-0145](../docs/adr/0145-read-only-what-this-release-writes-and-version-every-changed-shape.md).
