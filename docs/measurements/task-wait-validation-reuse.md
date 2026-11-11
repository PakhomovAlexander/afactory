# Task wait validation reuse

This change implements the [Task observation decision](../adr/2026-11-10-reuse-validated-artifacts-for-task-observation.md).
It makes no speedup claim: compare baseline and candidate built for the same target before drawing
one. GNU and musl are separate comparisons. Shared-host profiles are diagnostic, and overlapping
projection, decode, verification and canonicalization sample fractions must not be summed.

The unchanged waiter verifies current artifact bytes on every poll, reuses fully validated decoding,
and avoids cloning its projection or reconstructing its display while its lease remains live.
The optional last Gate hint uses indexed execution discovery and validates competing Tasks.
The tradeoffs are bounded retained decoded data and continued reads proportional to the observed
artifact closure; this is neither metadata-only integrity nor a zero-I/O wait.

The `cli_task_wait` case launches the real CLI, with its real startup, polling, final status and
exit. Its independent oracle requires exit 124 and `af/task-status@2` with a running Task. The
two-second timeout starts after `open_task`; display construction and observation run under that
clock, and final status serialization follows the timeout test. Whole-process wall time includes
work outside that timeout clock and can exceed
two seconds. It is not a latency assertion that the whole CLI exits in two seconds.

The registry preserves `cli_state_10000` and adds these controlled fixtures:

| Fixture | Current Task events | Authority package | Historical Tasks |
| --- | ---: | --- | ---: |
| `cli_wait_small_100` | 100 | Small | 0 |
| `cli_wait_small_10000` | 10,000 | Small | 0 |
| `cli_wait_large_10000` | 10,000 | `issue_255_large` | 0 |
| `cli_wait_small_history_100` | 10,000 | Small | 100 |
| `cli_wait_large_history_100` | 10,000 | `issue_255_large` | 100 |

The heavy author package is the existing 5,605,230-byte typed-envelope workload. Historical Tasks
share the Store and source identity, have released leases and no execution records. The current
plan declares a check. This measures startup and observation with unrelated historical Tasks
without inventing historical check measurements; the optional Gate hint belongs to plan preview,
and is not part of `task wait --json`. Separate history tests
cover measured check ordering, identity, cache annotations and corrupt evidence.

The original renewal coverage is retained separately from waiter setup. `state_projection_v2`,
`state_projection_seed_v1` and `cli_state_fixture_v1` write open, propose, admit and exactly
9,997 production renewals through the protected writer. The full `task show` oracle checks
all 10,000 sequences, numbered 0..9999 once each in order, and exactly 9,997 `lease_renewed`
transitions. Fixture setup observes the live durable expiry and waits only when the current
host millisecond cannot advance it. Each eligibility check has a one-second monotonic bound;
expiry/fencing fails promptly, and every protected write is attempted once. This coordination
is outside measured CLI/projection regions. Projection timing boundaries remain unchanged.

All six waiter fixtures instead use pause/resume setup with live release/take handoffs every
64th pair, then admit and take a fresh normal 60-second lease as the final write. The
`cli_state_10000` waiter uses the separately named `cli_wait_original_fixture_v1` writer;
it preserves the two-node authority and its existing waiter workload without changing the
renewal seed used by `task show`. The other five use `cli_wait_fixture_v1`.

The persistence diagnostic again appends exactly 100 production renewals after one setup
renewal (a four-event prefix), with FULL-synchronous SQLite transactions and complete ordered
replay. Its `persistence_fs_v3` contract explicitly includes clock-eligibility reads/waits in
the measured region: these renewals require advancing milliseconds even on a fast filesystem.
It preserves renewal coverage and semantic oracles, but is not timing-comparable with
`persistence_fs_v1`. The discarded `persistence_fs_v2` handoff workload is not a substitute
for renewal coverage. No writer rule, retry bound, budget or timing tolerance is relaxed.
See the [restoration decision](../adr/2026-11-11-preserve-renewal-performance-workloads.md).

Setup is outside the measured CLI process. No background writer, Provider inference,
longer-than-production lease or relaxed envelope cap is involved. Historical GNU/shared-host
numbers predate reconciliation and this restoration; they remain diagnostic history, not
calibrated or current-head measurements.

`total_cpu_ms` sums user and system nanoseconds from the same reaped child's rusage before exact
decimal conversion. The process runner and comparator use `reaped_child_rusage` for that metric.
`wall_ms` uses `child_process_lifetime`; `wait4_maxrss_bytes` is platform-converted RSS.
`cgroup_peak_memory_bytes` is separate and includes accounted cache. Missing required CPU data fails
the sample; wall time never substitutes for it. These use the existing metric-map contracts.

On a measurement host with the declared containment and toolchains, freeze exact same-target
comparisons from a Git checkout, using the reviewed authority that contains these workloads:

```sh
python3 -I scripts/performance/cli.py list
python3 -I scripts/performance/cli.py plan --repo . --trigger manual \
  --scope partial_scope --tier nightly --base BASE_SHA --head CANDIDATE_SHA \
  --authority AUTHORITY_SHA --case cli_task_wait \
  --environment linux_x86_64_gnu_claw --run-id wait_gnu --output /tmp/wait-gnu.json
```

Repeat the plan with `linux_x86_64_musl_claw` and a distinct run ID. Use `cli.py build`, `run`,
`aggregate` and `report` with those frozen plans as described in the
[laboratory guide](../design/continuous-performance.md). Retain both binaries, raw samples, total
CPU, wall and RSS distributions, setup receipts and oracle outcomes. Use `cli.py profile --help`
for the diagnostic child of the measured plan; profiles explain work but do not decide a speedup.

A frozen laboratory plan requires each side's probe to implement its declared workload contracts;
an older probe cannot stand in for a new writer contract. For the initial before/after CLI
diagnostic, the public writer below can prepare a fresh portable fixture for each same-target CLI.
Keep that setup outside the measured process, retain the writer's identity and label the results
diagnostic; such samples do not substitute for a frozen plan's side-specific writer receipts.

For a focused investigation outside a calibrated verdict, the writer can also be driven directly:

```sh
mkdir -p /tmp/wait-sample/waiter /tmp/wait-sample/repo
python3 -I fixtures/performance/cli/waiter-state.py \
  /tmp/wait-sample/waiter /path/to/same-target/perf-probe cli_wait_large_history_100
```

Launch that target's `af task wait perf-probe-task --timeout 2 --json` from the private repository,
with `--state /tmp/wait-sample/waiter/probe-work` and private HOME/XDG directories as the laboratory
supervisor supplies. Recreate the fixture immediately before each launch. A missing host capability
is an unavailable measurement, not a pass.
