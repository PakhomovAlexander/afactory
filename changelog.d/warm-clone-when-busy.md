- This repository's Task gate no longer waits for a warm layer another gate holds: its code
  policy declares `[warm] clone_when_busy = true`, so a gate that finds the layer busy clones it
  into its own runtime directory and rebuilds the workspace crates there (#459,
  [ADR-0207](docs/adr/0207-key-warm-check-layers-by-content-lock-them-split-the-build-deadline-and-seal-without-generated-files.md)).
