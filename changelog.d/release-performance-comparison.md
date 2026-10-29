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
  (`harness_last_write_to_command_exit`), never from file timestamps; a negative drain is kept as
  measured and leaves the gate `not_judged`. Each side is measured on the fixture contract its
  `af --version` selects: v0.13.0 runs the same packages declaring `af/TaskCheckReceipt@1`,
  re-pinned to declared digests its own `af catalog digest` must print. The documents are now
  `af/telemetry-overhead@2` and `af/telemetry-invocation-cost@2`; `@1` is refused.
- A 10,000-event seed writer (the native `state_projection` seed and the `task show` and
  Campaign CLI seeds) prepares its Store in a bounded 512 MiB tmpfs at `/perf/prep` through the
  same protected public writers, then publishes it, every file flushed, to the seed, where a
  fresh Store and the seed's oracle read it.
