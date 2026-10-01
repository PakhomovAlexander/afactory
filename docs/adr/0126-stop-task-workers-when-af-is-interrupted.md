# ADR-0126: Stop Task Workers when af is interrupted

**Status:** accepted (2026-10-01). Amends
[ADR-0089](0089-interrupt-task-work-when-its-writer-heartbeat-fails.md): the CLI now installs
interrupt handling, and an interrupt requests the same cancellation a failed heartbeat does.

## Context

On 2026-09-27, `<C-c>` on `af task run` ended af but left the running Codex reviewer alive. Its
process group had to be killed by hand before the Task could resume
([issue #135](https://github.com/PakhomovAlexander/afactory/issues/135)).

Outside the browser, af installed no SIGINT or SIGTERM handling. A terminal signals only its
foreground process group. `review-process` starts every Worker in its own process group
([ADR-0026](0026-share-process-supervision-through-a-leaf-crate.md)), so Workers never saw the
signal and outlived af. [ADR-0087](0087-control-native-task-invocations-through-the-shared-supervisor.md)
and ADR-0089 already give each CLI execution a cancellation flag. Supervision honours that flag
by killing and reaping the owned process group. ADR-0089 explicitly left CLI signals unwired.
The workspace forbids `unsafe` code, so af cannot install a classic signal handler.

## Decision

`af task run`, `af task start --execute`, `af review run` and `af provider doctor` take over
SIGINT and SIGTERM. Those are the commands that run Task work through the shared host. The
main thread blocks both signals before any other thread starts, so every later thread inherits
the mask. One watcher thread takes them with `sigwait`, so no code runs in signal context. The
standard library resets the signal mask before `exec`, so Workers start with their usual
signal state.

The first signal is recorded and forwarded to each execution's existing ADR-0089 cancellation
flag, including executions started after it. A scoped forwarder sets the same flag the heartbeat
sets. Supervision then kills and reaps each Worker process group with the existing bounded
drain. Nothing new reaches the supervisor or adapters; the flag remains local process state,
not a captured input, permission or allowance.

The interrupted Attempt settles through the existing path: `Failed`, with a diagnostic saying
the host cancelled it, and with its existing charge. It is never `Succeeded` and never left
pending. The run report is recorded as before. The host then refuses to continue. It does not
assemble or finish a TaskResult, finish a planner as incomplete, publish a Review Round
conclusion, or report Provider admissions. It releases the lease as on any error, so
`af task run` resumes the Task. No new domain-level cancelled state is added, and Task limits
are not refunded. A resumed run needs one more Attempt within its slot, pipeline and Task limits.

`review-process` lists the leader of every supervised process group from spawn until just before
its reap. A second signal while stopping kills every listed group with `SIGKILL` and exits at
once. Unlisting before the reap keeps the id reserved while it is listed. A full list only means
a group's own cancellation and deadline remain its stop.

af then ends with the conventional status: 130 for SIGINT and 143 for SIGTERM. It raises the
same signal at itself, still with the default action af never changed, so a shell reports
128 plus the signal number. A calling script or the browser's hand-off sees an interrupt, not an
ordinary failure. Where the environment ignores the signal, af exits with that status instead.
Stderr says the Task was interrupted and how to resume it: `af task run TASK_ID` when the
common Task is known, otherwise the same command again. With `--json`, a failed command's
`af/error@1` document carries that message and status.

## Considered options

- A `sigaction` handler that sets the flag directly needs `unsafe`, which the workspace forbids;
  `sigwait` on a dedicated thread is safe and runs ordinary code.
- One process-wide cancellation flag shared by all hosts would let one execution's heartbeat
  failure cancel an unrelated one. Forwarding into each execution's own flag keeps ADR-0089's
  scope.
- Finishing the interrupted Task as failed would end it and make the user restart it. Leaving
  the Attempt pending would hide the cancellation until lease expiry. Settling it as failed
  and releasing the lease keeps both the record and resumability.
- Killing the listed groups on the first signal would skip the supervised drain and reap. That
  loses the retained output and usage ADR-0087 and ADR-0088 keep, so it is only the second
  signal's path.
- Handling signals in every command would delay Ctrl-C for commands that start no Workers.
- `exit(130)` after cleanup gives the same number but hides the interrupt from a parent that
  checks how af ended. A shell loop would continue, and the browser would report an exit
  instead of `killed by SIGINT`.

## Consequences

An integration test runs a Task whose command Worker starts a long-running child. It sends
SIGINT to af and checks by pid that the Worker and child are gone. It also checks that af ends
by SIGINT (130 in a shell) and the Attempt settled `Failed` with a cancellation diagnostic. The Task stays unfinished,
and a later `af task run` resumes it to acceptance. Companion tests cover SIGTERM (143) and two
SIGINTs in a row. Existing supervision and heartbeat tests are unchanged.

A signal received while no execution is running is applied when the next execution starts, or
once the command returns. A non-cancellable step, such as source capture, completes first; a
second signal remains the immediate exit. Commands without Task work keep the default signal
behaviour.
