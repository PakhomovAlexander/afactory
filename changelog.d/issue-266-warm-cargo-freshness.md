- Native Task checks and measurements now refresh the mtimes of regular files in their private
  source trees before running. Reusing a warm Cargo target across Snapshots, including an
  A→B→A reversion, rebuilds workspace crates from the current Snapshot while keeping registry
  dependencies warm. Sources are ordered after the held target's actual mtimes without a future
  lead; future-dated targets run cold while Cargo home stays warm. Repeated Cargo invocations
  within a check reuse the units just built. Warm diagnostics still report available target bytes rather than a
  compiler cache hit or a previously built Snapshot (#266,
  [ADR-0161](docs/adr/0161-refresh-native-check-source-mtimes-before-reusing-cargo-target.md)).
