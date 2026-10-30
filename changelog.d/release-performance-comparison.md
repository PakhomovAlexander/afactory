- The performance laboratory's local executor plans its serial schedule: a core submission
  whose build, sampling windows and finish reserve cannot fit the run deadline is refused before
  any original, claim or build, naming the shards that fit; `cli.py executor preflight` prints
  the same checks as `af.perf-executor-preflight/1` without reserving anything.
- `cli.py executor overview --scope full|partial` writes one combined JSON, Markdown and HTML
  overview of a release comparison's originals (`af.perf-comparison-overview/1`), keeping state,
  evidence verification, coverage and the performance verdict apart. A `full` comparison needs
  core, telemetry, trace and CPU profile originals, a core aggregate that is not `incomplete`
  and profiles whose parent is its core; a queued, refused, partial or unavailable original
  makes it `incomplete`, and a `partial` scope is never `complete`. `status`, `verify` and
  `overview` read the journal without its lock, so they work while a `cycle` executes.
- A profile with `diagnostic.cases = []` diagnoses the regressions of a verified parent core
  original (`submit --work profile --parent REQUEST_ID`), bound to its exact plan and verdict
  (`af.perf-executor-request/2`); `diagnostic.perf` and `diagnostic.inferno_bin`
  (`af.perf-executor/3`) reach the native CPU capture, and missing tools refuse the profile
  before any build.
- `diagnostic.observer = "host_perf"` records a contained CPU profile with a trusted host
  observer: the operator's perf runs on the host under the authority supervisor, attached by
  frozen PID and start time to the owned container's gated process and acknowledged before the
  gate lets the command execute, so a millisecond CLI command is recorded whole and no Docker
  client time is attributed to AF; the candidate keeps its unprivileged container and never
  sees perf. Each recording keeps `af.perf-host-observer/1`, and the capability is checked by
  an actual attachment before any build, never from `perf_event_paranoid`.
- `cli.py executor comparison-preflight|comparison-launch|comparison-run|comparison-advance|
  comparison-status|comparison-report|comparison-export|comparison-verify` run one durable full
  release comparison (`af.perf-comparison/1`): the whole core family's frozen plan partitioned
  into sequential bounded core originals (later ones reuse the first one's verified build), the
  telemetry original, and trace and CPU profile lanes on the recipes of the plan's target,
  selected by the aggregate verdict (`af.perf-comparison-aggregate/1`). The report
  (`af.perf-comparison-report/1`) keeps completeness, verification, the verdict and calibration
  eligibility apart; refusals are retained; the export is a self-contained bundle with a hash
  manifest (`af.perf-comparison-export/1`) that `comparison-verify` checks after copying.
- The telemetry overhead and invocation-cost drain latency now runs from the harness's last
  write to the command's observed exit, both read on `CLOCK_MONOTONIC`
  (`harness_last_write_to_command_exit`), never from file timestamps; a run whose physical drain
  duration reads negative is kept as measured and leaves the gate `not_judged`, while a negative
  `spans` minus `off` difference is a judged signed comparison. Drain figures recorded at the
  drained result's CAS write are historical and not directly comparable. Each side is measured
  on the fixture contract its `af --version` selects: v0.13.0 runs the same packages declaring
  `af/TaskCheckReceipt@1`, re-pinned to declared digests its own `af catalog digest` must print.
  The documents are now `af/telemetry-overhead@2` and `af/telemetry-invocation-cost@2`; `@1` is
  refused.
- A 10,000-event seed writer (the native `state_projection` seed and the `task show` and
  Campaign CLI seeds) prepares its Store in a bounded 512 MiB tmpfs at `/perf/prep` through the
  same protected public writers, then publishes it, every file flushed, to the seed, where a
  fresh Store and the seed's oracle read it.
- A later comparison partition runs in its own bounded run window, sealed in its run bundle as
  `af.perf-partition-execution/1` and bound to the first partition's unchanged build and its
  recorded run, instead of inheriting the first partition's expired deadline; a forged, widened
  or wrong-partition authority starts nothing, and the aggregate checks each shard against its
  own partition's window
  ([ADR-0265](../docs/adr/0265-bind-comparison-partition-execution-to-reused-build-artifacts.md)).
- Preflight, submission and the claim share one storage and one time admission: the native
  reserve against `max_bytes`, and a telemetry or profile original's sequential bounds (both
  telemetry builds' caps, scratch, the profile's timeout) against `run_seconds`, each refusal
  naming the required and available figures before any claim or build. A telemetry original
  left too little time after its sources ends `failed`, `not_executed`, before either build.
- The CLI oracle declares each release's `af review report --format json` schema from its
  version (`@4` through 0.13, `@5` from 0.14.0-rc.1, `af_cli_v3`), and a CLI sample keeps its
  stdout within the unchanged `sample_log_bytes` total less a 16 KiB stderr reserve, so the
  whole 100-Finding report is retained and judged; a truncated output is refused as truncated.
- `comparison-export` carries every original's whole native evidence (core run bundles and
  reports with their evidence, the telemetry run with its manifest, raw documents and logs,
  the aggregate's run) and refuses a bundle whose pages link anything it does not hold;
  `comparison-verify` checks every bundled page's links and verifies the native evidence again
  offline against each original's sealed result.
- The host CPU observer admits each perf for the observed command's own bound plus its
  attachment and settlement allowances, within the 120-second per-run cap, instead of the whole
  remaining diagnostic window the supervisor refused; a failed attachment after the command
  started releases its scratch only on the supervisor's confirmed cancellation and keeps the
  observer's own reason ahead of any cleanup uncertainty.
- Executor preflight, submission and the claim check the configured `build_image` and
  `sample_image` (pinned, present, Linux, no implicit volume) and, in the build image under the
  machine exclusion, the recipe's Rust toolchain and target and the musl recipe's
  `zigbuild_bin` cargo-zigbuild and Zig versions; a missing image or tool refuses before any
  original, claim or build, and one gone after submission ends the original `failed` before
  its first build, with no fence kept.
- Storage admission also needs the journal root's filesystem to have the native reserve
  available now; the refusal prints the `max_bytes` figures and the filesystem's available
  bytes, and a large filesystem never widens `max_bytes`.
- A diagnostic CLI case keeps its sample's whole 262,144-byte log (245,760 bytes of stdout and
  its stderr), as core does, instead of adding another 64 KiB of stderr.
- The musl CPU profile recipe (`linux_musl_cpu_profile`) builds with line-tables-only debug
  information and frame pointers, as the GNU recipe does, so its AF and probe fit the unchanged
  build-artifact and symbol-evidence caps; full debug information left no diagnostic build.
