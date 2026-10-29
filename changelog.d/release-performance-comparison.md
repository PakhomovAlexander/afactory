- The performance laboratory's local executor plans its serial schedule: a core submission
  whose build, sampling windows and finish reserve cannot fit the run deadline is refused before
  any original, claim or build, naming the shards that fit; `cli.py executor preflight` prints
  the same checks as `af.perf-executor-preflight/1` without reserving anything.
- `cli.py executor overview` writes one combined JSON, Markdown and HTML overview of a release
  comparison's core, telemetry and profile originals (`af.perf-comparison-overview/1`), keeping
  state, evidence verification, coverage and the performance verdict apart; a queued, refused,
  partial or unavailable original makes it `incomplete`, never a pass.
- A profile with `diagnostic.cases = []` diagnoses the regressions of a verified parent core
  original (`submit --work profile --parent REQUEST_ID`), bound to its exact plan and verdict
  (`af.perf-executor-request/2`); `diagnostic.perf` and `diagnostic.inferno_bin`
  (`af.perf-executor/3`) reach the native CPU capture, and missing tools or a kernel that denies
  every unprivileged perf event refuse the profile before any build.
- The telemetry overhead and invocation-cost drain latency reads both ends from the one
  filesystem clock (`last_write_fs_ns`), so a run's drain is ordered; a negative drain is kept as
  measured and leaves the gate `not_judged`.
