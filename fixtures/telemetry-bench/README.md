# Telemetry bench fixtures

The pinned workload of Telemetry's overhead gate (design §12 of
[`docs/design/telemetry.md`](../../docs/design/telemetry.md); how to run it is the `Performance`
section of [`docs/telemetry.md`](../../docs/telemetry.md)). `scripts/telemetry-overhead.py` runs
each fixture's command once at `[telemetry] level = "off"` and once at `"spans"` per pair, each in
a fresh copy of the fixture's `repo/`, committed, with private homes and its own Store. Every
fixture is credential-free and spends no model tokens: its Workers are scripted harnesses, command
Workers the Task runtime supervises through the same `ModelRunner` capture as a Provider's model
process, and nothing reaches a network.

| Fixture | Command | What it exercises |
| --- | --- | --- |
| `telemetry-bench-concurrent` | `af task start --execute --file task.json` | Four concurrent Attempts: the Pipeline `bench/concurrent` runs four reviewer nodes with `max_parallel = 4`, each a scripted harness that writes 200 stream-json-shaped lines of 1 KiB to stderr, one every 5 ms, then answers a clean result. Telemetry's spans, the consumer thread and the segment writer under concurrent Attempts. |
| `telemetry-bench-dense` | `af task start --execute --file task.json` | One Attempt whose scripted harness answers first, then writes 50 MiB of stream-json-shaped lines to stderr at pipe speed, exercising the supervisor's complete-stream capture and telemetry tap. Its last write is one line naming the unix nanosecond it was made at and the same moment on the host's `CLOCK_MONOTONIC` (`last_write_monotonic_ns`); with the measurement's own `CLOCK_MONOTONIC` reading when the command's exit is observed, it gives the drain latency (`harness_last_write_to_command_exit`). |
| `telemetry-bench-store` | `af task start --execute --file task.json` | Two sequential native Review Tasks: 500 checks in 16 check nodes per Task, then four concurrent reviewers reporting 25 minor Findings each. Each final receipt covers all required named checks. Canonical Round publication retains every Report and Finding. Minor Findings leave the `major` gate clean, so the run exits 0. |

`harness.py` (identical in the two Task fixtures) and `reviewer.py` read their parameters from
`bench.json` beside them; `--scale smoke` rewrites only those parameters (20 paced lines, 1 MiB,
10 Findings per reviewer) and retains ten checks in one Store check node, with exactly those
checks in its policy. Its copied Pipeline package is re-pinned with `af catalog digest`.

One explicit `native-task-review@1` contract governs all five fixtures. Before timing, the
measured binary must reproduce every committed package pin and plan the exact workload as a
current zero-Attempt Task. Source preflight also checks both retained source trees against the
current required schema identities and constraints, without treating top-level prose annotations
as capability. Release strings are recorded as provenance, not capability proof;
there is no historical receipt rewrite, package overlay or silent fallback. Both sides run the
same current workload. Unsupported or malformed workloads preserve their original diagnostics.

The Store workload retains 1,000 actual checks and now records 200 Findings at full scale.
Each of two sequential native Tasks runs 500 checks and four reviewers. Thirty-one check/select
nodes plus bind, four reviewers, reducer and input assembly fit the unchanged 64-node bound.
Fourteen check nodes run 32 named checks, the next runs 20, and the last runs all 32. A typed
Select after each conditional check preserves failure/inconclusive receipts on every path, so
later checks and reviewers cannot bypass a predecessor's failure. Each command Worker's declared
wall is 50 seconds; the existing Task verification reserve remains 300 seconds. No deadline or
kernel bound is widened.

At smoke scale each Task runs five checks and four reviewers with ten Findings each: ten checks
and 80 Findings total. Both Task finalizations are timed; peak RSS is the maximum of their `wait4`
high-water marks. The first failed Task stops the sequence and retains its original diagnostic.
Native transitions and canonical Round outputs replace Campaign event publication, so the script
counts Store events from each run rather than assuming the former event count.

Every fixture has a pinned `.af/task-catalog.toml`. Recompute an edited package's digest with
`af catalog digest PACKAGE_DIR`; the benchmark refuses a mismatch. No fixture needs a Campaign
Pipeline or `.af/af.lock`. Historical AF release installation and dispatch remain supported by
self-management; they are independent of current benchmark workload admission.

## Invocation cost

`scripts/telemetry-invocation-cost.py` measures what the Claude `stream-json` invocation costs
against `json` (design §12, the invocation-cost gate; the result is under `Performance` in
[`docs/telemetry.md`](../../docs/telemetry.md)). Its two fixtures run one Task through the Task
runtime whose Workers are Claude model Workers bound to the Provider `claude-bench`:
`telemetry-bench-invocation-one` (one reviewer) and `telemetry-bench-invocation-four` (four
reviewers with `max_parallel = 4`). Their harness is
[`invocation-claude.py`](invocation-claude.py), installed as `claude` on each command's `PATH`
with a private Provider registry naming its auth directory; it answers af's help, version,
login and catalog checks and the admission probe, and runs one scripted Attempt per request.
`bench.json` in the auth directory, written per run, names the shape it prints and the bytes of
intermediate tool output (64 KiB tool results), so the repository and its Snapshot are the same
for both shapes; under either it builds the same output and answers the same final result. The
pins in each `.af/task-catalog.toml` are recomputed with `af catalog digest PACKAGE_DIR` after a
change. The `af` integration tests of the content level also set optional keys the measurements
never set: `record_stdin` (a file in the auth directory the request is written to as received),
`thinking` (thinking blocks under `stream-json`), `say` (a text block before the final reply)
and `tool_output_prefix` (text that begins the first tool result).
