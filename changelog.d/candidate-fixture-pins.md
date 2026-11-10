- Validate staged triage package pins with the current source build, using Cargo's configured
  target directory. An installed `af` on `PATH` can no longer select an older contract reader;
  missing build output fails explicitly. Digest assertions, private state and offline behavior
  remain required. Refresh the performance CLI receipt overlay's nested pins to the current
  Pipeline and TaskKind packages. See the
  [current-format consolidation](../docs/design/current-contracts-and-task-review.md).
