- A Task whose `af task run` died mid-Attempt (a lid closed on battery, SIGTERM, a reboot, a
  killed process, a lost lease) resumes with `af task run TASK_ID`: the lost Attempt is settled
  as abandoned, charged only the usage its Provider reported (never its reservation, which goes
  back to the Task's budget), and its node is dispatched again without using one of its
  Attempts, once per node unless its slot declares `recovery_attempts` (`af.pipeline/3`, compiled
  as `af.compiled-task/2`). SIGINT and SIGTERM settle the interrupted Attempt the same way, so the
  advertised resume now runs the node again. The resume's first line says which node runs again,
  or `cannot continue Task X: node N lost its writer 2 times (recovery allowance 1); fix: …`
  with exit 1; a dead run's lease is waited out with a line saying so instead of refused
  ([ADR-0197](docs/adr/0197-recover-a-lost-or-interrupted-attempt-without-charging-its-reservation.md)).
- The writer lease is 60 s, renewed by its own heartbeat thread once a third has passed; a writer
  that loses it stops its Workers at once and says `lost its writer lease (writer_lost: …)` with
  the Store's observed time and the lease's expiry. On macOS `af task run` and
  `af task start --execute` hold a power assertion (`caffeinate -i -s -w`) while they run, and
  `af task run --verbose` says so.
- `af task show` prints an interrupted Task as `phase abandoned (cause writer_lost)` or
  `(cause host_cancelled)` with `next: af task run TASK_ID`, and `af task list` as
  `abandoned (…); next: …`; their documents are `af/task-inspection@12` and
  `af/task-list-entry@3`, with `cause` and `next_action`. The refusal `Task reservation is not
  dispatchable` names the guard that fired and its numbers (`reservation 40 > remaining 129`).
