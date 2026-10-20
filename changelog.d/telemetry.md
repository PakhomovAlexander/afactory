- Telemetry, first package: a new leaf crate, `review-telemetry`, records what `af` does in its
  own store under `$XDG_STATE_HOME/af/telemetry/`, never in a Task or Campaign Store, and the
  kernel never reads it. The machine-only `[telemetry]` table (`level` = `off`, `metrics`,
  `spans` or `content`, and its bounds; `AF_TELEMETRY__<KEY>`) is ignored and reported in a
  repository's `.af/af.toml`, and `af help telemetry` documents it. `af telemetry status` and
  `af telemetry reindex` (`--json`: `af/telemetry-status@1`, `af/telemetry-reindex@1`) show and
  rebuild the store; `status` accounts every byte beneath it, a path it does not recognize
  under an `unrecognized` entry it never reads or removes, and rechecks each registered
  directory's overlap, `unknown` when the Store registry cannot be read. The credential rules for Provider-authored text moved into the crate
  unchanged, with key-aware JSON redaction added; only a kernel identity in an identity position
  escapes them, and `status` re-reads a sealed segment changed under its name. No command records
  spans yet
  ([ADR-0153](docs/adr/0153-record-telemetry-in-its-own-store-and-never-let-it-steer.md)).
- Telemetry, second package: the commands that run a kernel run (`af task run`,
  `af task start --execute`, `af review run`, `af provider doctor`, `af self optimize --execute`)
  now record `af.command > af.run > af.authority.load, af.plan` (one `af/TelemetryNode@1` record
  per plan node), `af.node > af.attempt` with the kernel's exact accounting, and the run's
  `af.sweep`; every other command, and every command at `off`, opens no telemetry directory. One
  consumer thread owns the segment while a run is open and makes every record visible within
  `flush_latency_ms`; it is stopped within 2 s or abandoned, leaving the segment for the next
  reader to recover. `af telemetry runs`, `show`, `tail` and `gc` (`--json`:
  `af/telemetry-runs@1`, `af/telemetry-show@1`, `af/telemetry-tail@1`, `af/telemetry-gc@1`)
  report a run's executions — active time as their union, execution time as their sum, elapsed
  time, history `prefix:k` or `partial`, never complete — its span tree and its live tail.
  Telemetry is the kind `telemetry` of the Storage Budget (`af/storage@2`,
  `af/storage-prune@2`): every registered directory counted once, one entry per segment by what
  its eviction frees, evicted under ADR-0144's rules past `[telemetry] max_bytes` or the whole
  budget, with temporaries, unreferenced blobs and free index pages reclaimed by every applied
  sweep and an overlapping directory reported `overlapping`, never evicted as telemetry.
