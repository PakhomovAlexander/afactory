- Telemetry, first package: a new leaf crate, `review-telemetry`, records what `af` does in its
  own store under `$XDG_STATE_HOME/af/telemetry/`, never in a Task or Campaign Store, and the
  kernel never reads it. The machine-only `[telemetry]` table (`level` = `off`, `metrics`,
  `spans` or `content`, and its bounds; `AF_TELEMETRY__<KEY>`) is ignored and reported in a
  repository's `.af/af.toml`, and `af help telemetry` documents it. `af telemetry status` and
  `af telemetry reindex` (`--json`: `af/telemetry-status@1`, `af/telemetry-reindex@1`) show and
  rebuild the store. The credential rules for Provider-authored text moved into the crate
  unchanged, with key-aware JSON redaction added. No command records spans yet
  ([ADR-0151](docs/adr/0151-record-telemetry-in-its-own-store-and-never-let-it-steer.md)).
