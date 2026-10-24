# ADR-0203: Recover a lost or interrupted Attempt without charging its reservation

**Status:** accepted (2026-10-23). Supersedes in part
[ADR-0143](0143-charge-zero-and-record-unknown-usage-when-no-usage-is-reported.md): the
reservation floor of an abandoned Attempt whose usage was reported, and that an abandoned
Attempt counts against the Attempt limits. Supersedes in part
[ADR-0129](0129-stop-task-workers-when-af-is-interrupted.md): an interrupted Attempt settles
`Failed`, and a resumed run needs one more Attempt within its slot. Amends
[ADR-0128](0128-renew-a-live-task-writer-lease-through-its-own-connection.md): the 15 s lease
and the 10 s renewal threshold.

## Context

Issue #265, in 13 recorded runs on a Mac and on the claw-3 VM: when the `af task run` process
dies during a Worker Attempt (a lid closed on battery, SIGTERM from a launcher, a VM reboot, a
lease renewal starved by work), the Task is dead.

- The next writer settles the orphaned Attempt as abandoned, and an abandoned Attempt counts
  against its slot. Every `implementation-reviewed` slot has `max_attempts = 1`, so the resume
  printed `tokens 2333 (+1 unknown: lease expired)` and then `root.nodes.implement: event store
  conflict: Task node root.nodes.implement exhausted its Attempt limit`, with 9 Attempts and 4M
  tokens of the plan unused.
- An abandoned Attempt whose Provider had reported any usage was charged at least its whole
  reservation (ADR-0143's floor): research-r1 read `1 failed Attempt (1 abandoned; 1,500,000
  tokens)` for 21 minutes of work.
- The advertised SIGINT resume (`To continue, resume it with af task run X`) did not dispatch
  the node again for the same reason: ADR-0129 settled the interrupted Attempt `Failed`, and it
  used the slot's only Attempt. On claw-3, 27 Tasks stayed `running`.
- The lease lasted 15 s. A laptop's wake, App Nap's timer coalescing or a loaded machine
  (`telemetry_content_allowances_hold`) could take longer than the 5 s left between the renewal
  threshold and expiry, so a live writer was lost.
- Every launcher wrapped `af task run` in `caffeinate -i -s` by hand.
- `af task show` and `af task list` showed such a Task as running, and nothing said on its first
  line why a resume could not continue.

The owner's rules: a user whose Mac sleeps or whose client auto-updates must not lose a Task, and
`af task run` says on its first line what it does or why it cannot continue.

## Considered options

- **Raise every slot's `max_attempts`.** Rejected: it lets an Attempt that answered badly run
  again too, which the slot's bound exists to forbid, and the defaults of every package change.
- **Refund a lost writer's Attempt entirely, uncounted and unbounded.** Rejected: a node whose
  Worker kills af every time (a fork bomb, an out-of-memory Worker) would be dispatched forever.
  A separate, declared bound keeps the total finite: `max_attempts` plus the recovery allowance.
- **Keep the reservation floor for reported but incomplete usage.** Rejected by the owner's
  rule: an abandoned Attempt is charged what was observed, and the reservation only bounded it
  while it ran. A Provider that under-reports is shown as such by the observation it sent.
- **A new `af/TaskExecutionRecord@6` with an explicit cause on `Abandoned`.** Rejected: the
  cause is already a recorded fact. A later writer settles a lost writer's Attempt, and only the
  Attempt's own writer can settle it with that epoch, which it does only for a host
  cancellation. Reading the cause from the settling epoch changes no persisted shape and every
  existing log replays the same.
- **Let a lost writer renew its own expired lease.** Rejected as in ADR-0128: expiry would no
  longer mean that a successor may take the Task, and two writers could coexist.
- **Take over a lease whose holder's process is gone, before it expires.** Rejected: a writer is
  named `cli-<pid>`, which no other host can check and which the system reuses, so fencing would
  rest on a guess. Waiting out the lease is exact.
- **Call IOKit's `IOPMAssertionCreateWithName` directly.** Rejected: the workspace forbids
  `unsafe`. `caffeinate -w <pid>` takes the same `PreventUserIdleSystemSleep` assertion and
  ends by itself with af.
- **Add the abandoned state to `af/TaskPhase` (`phase: abandoned`).** Rejected for this package:
  the phase is persisted, and the state model of package P5 owns its vocabulary. The view adds
  only `cause` and `next_action`, derived from the log.

## Decision

1. **Heartbeat.** The lease is 60 s (`review_pipeline::task::lease::TASK_LEASE_MS`). The heartbeat
   runs on its own thread by its own tick of at most 1 s and renews once a third of the lease has
   passed, with two thirds left, so a writer survives 40 s without CPU. ADR-0128's connection
   rules hold unchanged at that cadence: renewal through the work's connection when it is free,
   through the heartbeat's own one in the last 4 s reserve, never below the 1.5 s floor. A
   heartbeat that cannot read or renew the lease flags the writer as lost, requests cancellation
   so supervision stops the Worker processes at once, and fails with an error that starts
   `Task writer lost its lease (writer_lost)` and states the Store's own observation: its time,
   the lease's expiry and the last recorded time, or the writer that now holds the lease.
   `LeaseTiming::for_lease` derives the same cadence for any lease of 6 s or more;
   `for_lease(15_000)` is ADR-0128's.
   The performance laboratory records production's lease as a product constraint and its probe
   holds that lease, so its registry follows: `task_lease_ms = 60000` in
   `fixtures/performance/suite.toml`, `TASK_LEASE_MS` in `perflab/suite.py`, `reference.py` and the
   probe's `fixtures.rs`, the regenerated golden constants and the re-pinned harness digest. No
   probe case waits a lease out, so no measured region grows; a registry that kept 15 s would
   measure a lease production no longer takes.
2. **A lost writer records nothing more.** Its runtime does not settle the interrupted Attempt,
   and cannot release the lease. The usage the Provider reported is already in the Attempt wall
   sidecar, written before settlement (ADR-0088).
3. **Abandoned, by cause.** The next writer settles a lost writer's started Attempt as
   `Abandoned`, as before; its diagnostic now records `cause: writer_lost`, the lost writer's
   epoch, its last recorded time, its lease's expiry and the time the loss was observed. A host
   cancellation (SIGINT, SIGTERM) settles the Attempt `Abandoned` under its own writer, with
   `cause: host_cancelled` in its diagnostic, instead of `Failed`. The projection reads the cause
   from the settling epoch: a later epoch is `writer_lost`, the Attempt's own is
   `host_cancelled`. `af/TaskExecutionRecord@5` keeps its shape.
4. **Charged its observed usage.** An abandoned Attempt is charged the usage reported before it
   stopped, or 0 with its usage unknown (ADR-0143), never its reservation, and its settlement
   releases the reservation. Replay admits an abandoned settlement below its reservation when it
   carries a usage report or the unknown-usage marker, and refuses one with neither.
5. **Counted against a recovery allowance.** An abandoned Attempt counts against no Attempt
   limit (its node's, its calls' or the Task's) and against its node's recovery allowance: one,
   unless the node's slot declares `recovery_attempts` (0 to 8). A node with more abandoned
   Attempts than its allowance is not dispatched again (`Task node N exhausted its recovery
   allowance: 2 abandoned Attempts, recovery allowance 1`). Tokens and the deadline still bound
   every Attempt.
6. **`af.pipeline/3` and `af.compiled-task/2`.** Under ADR-0145 clause 2 a slot's
   `recovery_attempts` is a new shape: `af.pipeline/3` (schema `pipeline-definition-v3.json`,
   definitions `pipelineV3`, `contractV3` and `slotV3` of `task-contracts-v1.json`, parity fixture
   `fixtures/task-contracts/v3/`) is a pipeline with a slot that declares it, with or without a
   Project Brief port, and the only version that may. An `af.pipeline/1` or `af.pipeline/2`
   pipeline declaring it is refused when it loads, naming the version it needs, and so is an
   `af.pipeline/3` pipeline that declares none. Its compiled graph carries the slot and is labelled
   `af.compiled-task/2` (schema `compiled-task-v2.json`); every other graph keeps
   `af.compiled-task/1` and its bytes, and a graph labelled otherwise than its shape is refused.
   A child slot bound to a parent slot may not allow more recoveries than it declares.
   `af.planning-request/1` already offers only `af.pipeline/1` pipelines to the Planner.
7. **The resume says first what it does.** `af task run` waits out a lease its previous run still
   holds, at most one lease length, saying so on its first line; a lease renewed meanwhile
   belongs to a working run, and the wait ends with `cannot continue Task X: another run is
   working on it`. After settling a lost writer's Attempts, each node that runs again gets one
   line, `resuming Task X: node N was interrupted at 17:41:03Z (its writer was lost); settling the
   lost Attempt and dispatching a new one (1 recovery of 1)`. A node past its allowance stops the
   run before anything runs, exit 1: `cannot continue Task X: node N lost its writer 2 times
   (recovery allowance 1); fix: start it again with af task start --file TASK_FILE ..., or allow
   more with [slots.S] recovery_attempts in its pipeline (af.pipeline/3)`. `af review run` and the
   planning resume wait out a dead run's lease the same way. The stale-sandbox sweep's line moves
   after the command's own output, so it never takes the first line.
8. **The abandoned view.** A Task is abandoned when it is unfinished, its lease is neither held
   nor renewed, and it has a started Attempt nobody settled or a node whose latest Attempt was
   abandoned and that has no output. Its state is P5's (`af help task-states`): phase
   `abandoned`, outcome `cancelled`, cause `writer_lost` or `host_cancelled` (an Attempt nobody
   settled, then the cause its last Attempt was abandoned with), and next command
   `af task run X`, or `af task start --file TASK_FILE` once a node used up its recovery
   allowance. `af task show` adds `why: node N stopped at T`. The documents carry it in their
   `state` object, `af/task-inspection@13` and `af/task-list-entry@3`; `phase` keeps the
   recorded phase. The inspection's graph is either compiled version.
9. **The dispatch guards name themselves.** The five guards of `TaskBudget::begin` that refused
   with `Task reservation is not dispatchable` now say which fired and its numbers: released,
   already settled (and its charge), a clock moving backwards (both times), a passed deadline
   (both times), or a breached budget (`charged 31 of a 200 token limit, reservation 40 >
   remaining 129`). The token guard of `prepare` states the reservation, the protected
   verification and what remains.
10. **A power assertion.** On macOS `af task run` and `af task start --execute` start
    `/usr/bin/caffeinate -i -s -w <af's pid>`, by absolute path, in its own process group, for
    their whole run; `af task run --verbose` says so on stderr, or why none is held. Elsewhere
    nothing is started.

## Consequences

- A Task whose Mac slept, whose launcher sent SIGTERM or whose process was killed resumes with
  `af task run X`, without being charged a reservation and without spending a slot Attempt, once
  per node by default. `crates/af/tests/it/task_recovery.rs` kills `af task run` with SIGKILL and
  sends SIGTERM mid-Attempt to a slot of `max_attempts = 1`, and the resumes finish with one
  chargeable and one abandoned Attempt; a third interruption is refused on line 1; a declared
  `recovery_attempts = 2` recovers twice. The Store's tests charge an abandoned Attempt its 3
  observed tokens of a 10-token reservation and release the rest; the heartbeat's tests keep a
  lease while the work blocks every Store write for three lease lengths, and stop a Worker within
  one lease when renewal cannot commit.
- A crashed run's successor waits up to 60 s instead of 15 s. It says so and waits instead of
  refusing.
- `af task report` still lists an abandoned Attempt among the failed ones, with its class
  `abandoned`; the outcome vocabulary is package P5's.
- An abandoned Attempt recorded by an earlier release keeps the charge it recorded; only new
  settlements follow this record. An earlier release refuses a log that holds an abandoned
  settlement below its reservation without the marker, which fails closed (ADR-0145).
- A slot that declares `recovery_attempts` cannot be offered to the Planner for composition until
  `af.planning-request/1` gains a version that carries it.
