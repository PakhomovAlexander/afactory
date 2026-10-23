# Interrupted runs: writer loss, interruption and recovery

A Task run can stop before its Task finishes: the laptop's lid closes on battery, a launcher
sends SIGTERM, a VM reboots, the process is killed, or its writer lease is lost. The next
`af task run TASK_ID` continues it. This page is the recovery contract: what is charged, what
runs again, how many times, and what `af task run`, `af task show` and `af task list` print. The
decision is [ADR-0197](../adr/0197-recover-a-lost-or-interrupted-attempt-without-charging-its-reservation.md).

Terms: a *writer* is the one `af` process that holds a Task's *lease* and may record on it; an
*Attempt* is one bounded run of a node's Worker or check ([CONTEXT.md](../../CONTEXT.md)
defines both).

## How a run stops

| What happened | What is recorded | Cause |
|---|---|---|
| `af` was killed (SIGKILL, power loss, reboot), or slept longer than its lease | Nothing more: its started Attempt stays unsettled until the next writer settles it | `writer_lost` |
| The heartbeat could not renew the lease | The Worker processes are stopped at once and nothing more is recorded; the next writer settles the Attempt | `writer_lost` |
| SIGINT (Ctrl-C) or SIGTERM | The Worker processes are stopped, the Attempt is settled as abandoned and the lease released | `host_cancelled` |

A run whose heartbeat lost the lease ends with:

```text
af task: Task pagination-cli lost its writer lease (writer_lost: Task writer lease is expired or fenced: observed at 1761241263000, expiry 1761241260000, last recorded 1761241240000); its Worker processes were stopped and nothing more was recorded. To continue, run `af task run pagination-cli`: ...
```

The times are what the Store observed: its clock, the lease's expiry and the last event the
writer recorded.

## The lease

The lease is 60 s, renewed by a heartbeat on its own thread, by its own one-second tick, once a
third of it has passed, whatever the Worker or the Store is doing. A writer therefore survives
40 s without any CPU: a laptop's wake from a short sleep, App Nap's timer coalescing and a loaded
machine's scheduling delays. A longer lease would only delay the takeover after a crash. Two
writers never hold one lease: a successor takes it only after it expired or was released.

On macOS `af task run` and `af task start --execute` also hold a power assertion for their whole
run, `/usr/bin/caffeinate -i -s -w <af's pid>`: the Mac does not idle-sleep, and on AC power does
not sleep, while a Task runs. `af task run --verbose` says so on stderr. A closed lid on battery
still sleeps; the recovery below is what keeps that from losing the Task.

## What is charged

An abandoned Attempt is charged what its Provider reported before it stopped, never its
reservation; with nothing reported it is charged 0 and its usage is recorded as unknown
(`lease_expired` or `interrupted`, [ADR-0143](../adr/0143-charge-zero-and-record-unknown-usage-when-no-usage-is-reported.md)).
Its whole reservation is released when it is settled, back into the Task's remaining tokens.

## What runs again

An abandoned Attempt is no answer of its node, so it counts against none of the Attempt
limits: not its node's `max_attempts`, not a child Pipeline's, not the Task's. It counts against
the node's *recovery allowance* instead, one abandoned Attempt per node unless the node's slot
declares more. Within the allowance, the next `af task run` dispatches the node again; an
interrupted check runs its check again. A node whose abandoned Attempts exceed the allowance
is not dispatched again, and the Task cannot continue.

A slot declares its allowance in its pipeline, which makes the pipeline `af.pipeline/3`:

```toml
schema = "af.pipeline/3"
# ...

[slots.implementer]
worker = "fixture/implementer"
role = "implement"
input_type = "af/ImplementationInput@1"
output_type = "af/ImplementationReport@1"
min_attempts = 1
max_attempts = 1
recovery_attempts = 2
allow_local_replacement = true
independent_from = []
```

`recovery_attempts` is 0 to 8. A pipeline whose slots declare none keeps `af.pipeline/1`, or
`af.pipeline/2` with a Project Brief port, and its bytes; an `af.pipeline/1` or `af.pipeline/2`
pipeline that declares one is refused when it loads, naming `af.pipeline/3`
([`pipeline-definition-v3`](../../schemas/pipeline-definition-v3.json)). Its compiled graph is
`af.compiled-task/2` ([`compiled-task-v2`](../../schemas/compiled-task-v2.json)). A child slot
bound to a parent slot may not allow more recoveries than the child declares.

## What `af task run` says first

A run that waits for a dead run's lease says so on its first line:

```text
resuming Task pagination-cli: its previous run's lease (writer cli-4242) is held until 17:42:03Z; waiting 41 s for it to expire
```

A lease renewed while it waits belongs to a run that is still working, and the wait ends with
`cannot continue Task pagination-cli: another run is working on it ...`. Then each interrupted
node gets one line:

```text
resuming Task pagination-cli: node root.nodes.implement was interrupted at 17:41:03Z (its writer was lost); settling the lost Attempt and dispatching a new one (1 recovery of 1)
resuming Task pagination-cli: node root.nodes.implement was interrupted at 17:41:03Z; dispatching a new Attempt (1 recovery of 1)
```

A node past its allowance stops the run before anything is dispatched, on its first line, with
exit code 1:

```text
af task: cannot continue Task pagination-cli: node root.nodes.implement lost its writer 2 times (recovery allowance 1); fix: start it again with `af task start --file TASK_FILE` (the Task file it was started from), or allow more with `[slots.implementer] recovery_attempts` in its pipeline (af.pipeline/3)
```

## What `af task show` and `af task list` print

A Task is *abandoned* when it is unfinished, no run holds its lease, and its last run left an
unsettled Attempt or a node whose latest Attempt was abandoned. `af task show` prints, after its
first line:

```text
phase abandoned (cause writer_lost): node root.nodes.implement stopped at 17:41:03Z
next: af task run pagination-cli
```

`af task list` prints `abandoned (writer_lost); next: af task run pagination-cli` in the state
column. Their `--json` documents, `af/task-inspection@12` and `af/task-list-entry@3`, carry
`cause` (`writer_lost` or `host_cancelled`) and `next_action`, both present exactly for an
abandoned Task; `phase` keeps the phase the Task recorded. A node past its allowance makes
`next_action` `af task start --file TASK_FILE`.

## Where it is implemented

| Part | Location |
|---|---|
| Heartbeat cadence, the lease and writer loss | `crates/review-pipeline/src/task/lease.rs` |
| Settling an interrupted Attempt as abandoned | `crates/review-pipeline/src/task.rs` |
| Settling a lost writer's Attempt, the cause and the recovery count | `crates/review-store/src/store/task/execution.rs` |
| Attempt limits, recovery allowance and the dispatch guards' refusals | `crates/review-attempt/src/task_budget.rs` |
| The resume's first line, the wait and the abandoned view | `crates/af/src/task_execution/recovery.rs` |
| The power assertion | `crates/af/src/power.rs` |
| Tests | `crates/af/tests/it/task_recovery.rs`, `crates/review-store/src/store/task/tests/recovery.rs`, `crates/review-pipeline/tests/it/task_runtime/control.rs` |
