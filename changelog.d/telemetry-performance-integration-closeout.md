- Contain telemetry experiments and collect only results verified under their authority. Every
  execution of `cli.py telemetry-run` runs through one executor. `--mode linux_container`, the
  only mode for a pull request, gives each execution its own container of the pinned image with
  only its own scratch directory writable, bounded while it writes. An uncertain cleanup or a
  changed binary, authority, corpus or result stops the run. A document is admitted only as its
  planned run, with every figure recomputed (read latency included), and a result verifies only
  under exactly its measurement authority. The workflow builds in the pinned image and uploads
  only what `telemetry-admit` verified and found credential-free. The new
  `cli.py telemetry-collect` lane keeps verified results in a locked, bounded history beside the
  unchanged archive collector
  ([ADR-0215](docs/adr/0215-contain-telemetry-experiments-and-collect-only-results-verified-under-their-authority.md)).
