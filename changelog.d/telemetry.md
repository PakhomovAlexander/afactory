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
  `af.sweep`; every other command, and every command at `off`, opens no telemetry directory. From
  the first `af.run` until the seal one consumer thread is the segment's only writer and makes
  every record visible within `flush_latency_ms`; the command's end and the seal are one request
  to it, bounded at 2 s, after which it is abandoned and publishes nothing, leaving the segment
  for the next reader to recover. Every lease that begins kernel execution now commits an
  execution marker to its Task's log, the event `TaskExecutionStarted@1`
  (`af/TaskExecutionStarted@1`) beside `TaskTransition@5`, at every telemetry level. `af telemetry runs`, `show`, `tail` and `gc`
  (`--json`: `af/telemetry-runs@1`, `af/telemetry-show@1`, `af/telemetry-tail@1`,
  `af/telemetry-gc@1`) report a run's executions — numbered by those markers, active time as
  their union, execution time as their sum, elapsed time, history `prefix:k` or `partial`, never
  complete, the open ones named beside the sealed ones in every label and total, the outcome the
  latest execution's own, by ordinal — its span tree and its live tail, which fails naming a sealed segment
  it cannot read and follows a run in one Store, refusing with `--store` guidance, once the
  followed Store's pending records are printed, a second Store that starts the same run id
  while it is followed, even in a segment sealed between two of its looks.
  Telemetry is the kind `telemetry` of the Storage Budget (`af/storage@2`,
  `af/storage-prune@2`): every registered directory counted once, one entry per segment by what
  its eviction frees, a sealed one last used at its seal time, evicted under ADR-0144's rules
  past `[telemetry] max_bytes` or the whole budget, every file of an entry, its manifest
  included, checked again before either is unlinked and the whole entry left in place, reported
  `changed`, when one is no longer what the inventory found, with temporaries, unreferenced blobs
  and free index pages reclaimed by every applied sweep and an overlapping directory reported
  `overlapping`, never evicted as telemetry.
