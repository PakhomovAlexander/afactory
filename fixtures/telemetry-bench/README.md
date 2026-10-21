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
| `telemetry-bench-dense` | `af task start --execute --file task.json` | One Attempt whose scripted harness answers first, then writes 50 MiB of stream-json-shaped lines to stderr at pipe speed: today the supervisor's complete-stream capture drains it, later the observer and tap of package P6 read it. Its last write is one line naming the unix nanosecond it was made at; with the drained stdout's store time it gives the drain latency. |
| `telemetry-bench-store` | `af review run --pipeline .af/pipelines/review.toml --policy-rev HEAD` | A Task-backed Review whose gate checks and reviewer nodes append Store events: 1,000 checks append one `CheckCompleted@1` each and four reviewers report 25 minor Findings each, one `FindingReported@1` apiece, beside the Task's own transitions — about 1,180 events per run, each its own commit. The minor Findings leave the `major` gate clean, so the run exits 0. |

`harness.py` (identical in the two Task fixtures) and `reviewer.py` read their parameters from
`bench.json` beside them; `--scale smoke` rewrites only those parameters (20 paced lines, 1 MiB,
10 Findings per reviewer) and keeps the first 10 of the Store fixture's checks, re-pinning its
copy's pipeline, which is how `make check` runs one pair of each.

The Store fixture appends about 1,180 Store events, not the 10,000 the design names. A Task's own
log grows by about five transitions per node, checks of a Task Pipeline add none, and a Pipeline
holds at most 64 nodes; only a Review's checks and Findings scale. Findings fold into the Round's
ledger in one Store-holding step whose time grows faster than their count, and once that step
outlasts the lease's renewal reserve the heartbeat renews through its own connection and the
conclusion is refused, at either level, with `event store conflict: Review conclusion changed
its exact Task accounting prefix`: 4,000 Findings took 54 to 67 s and failed, and 1,000 failed
once at a load average of 20. Checks commit one at a time and never meet that race, but each costs
about 10 ms: 9,900 of them took about 3 minutes a command at a load average of 50, so the gate's
40 runs of this fixture would take one and a half to two and a half hours. This fixture takes its
events from 1,000 checks and keeps its Findings few; 10,000 events is a later choice between the
gate's wall and the Review fold.

Every package digest in `.af/task-catalog.toml` and the pipeline digest in the Store fixture's
`.af/af.lock` are pinned: a change to a package file needs its pin recomputed with
`af catalog digest PACKAGE_DIR`, and a change to `.af/pipelines/review.toml` the lock's digest.
