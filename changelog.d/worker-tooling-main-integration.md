- Integrate main `3f5c9746bd31587742282b9fcfe17079ef236937` with Worker development tooling:
  retain verification Review acceptance, the Scenario merge watcher and RC4/RC5 telemetry
  contracts alongside Worker development v3, immutable caches and bounded knowledge retrieval.
  The 18 incoming-only files retain main's exact bytes and executable modes. The combined test
  catalog retains both sets of regressions and live probes; the performance policy and inventory
  are regenerated over the combined harness, including the Worker Rust fixture's Cargo inputs.
  The validated coordinator input bundle is removed from the delivered source.
