# ADR-0250: Follow a Task as a stream of its Store events

**Status:** accepted (2026-10-28). Adds the printed document `af/task-event@1`
(`schemas/task-event-v1.json`) and the command `af task events`
([ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)).
Extends [ADR-0211](0211-give-the-agent-loop-its-verbs-and-deliver-a-verified-verification.md)
and [ADR-0245](0245-show-recorded-gate-history-and-version-task-status.md).

## Context

An agent that runs a Task in the background can block on `af task wait` or poll `af task status`
(issue [#370](https://github.com/PakhomovAlexander/afactory/issues/370)). Neither says what
happened between two looks: which node started, which Attempt settled and what it was charged.
The progress lines of `af task run` say it, but as prose on stderr of the process that runs the
Task, and a detached run writes them to a log file an agent would have to scrape. A follower
also loses its connection: a sandbox's command limit ends it, or its host restarts it. It then
needs to continue where it stopped, without missing an event or reading one twice.

The Store already records every one of these facts in the Task's log, in order, each event with
a sequence that is dense and gapless within the log (a gap is refused on replay as loss).

## Decision

1. **A new verb, `af task events TASK_ID`.** It prints JSON lines on stdout, one
   `af/task-event@1` document per line, and follows the Task until it settles in the sense
   `af task wait` uses: no writer holds it and no run of this host is starting. It then prints
   one terminal record and exits with the Task's exit code, as `af task wait` does: its outcome's
   (`af help exit-codes`), 4 for a Task that waits for a person. A planned Task with no run in
   progress is refused (exit 1), as by `af task wait`. It reads the Store and changes nothing.
2. **An event line is one event of the Task's log.** `record` is `event`; `sequence` is the
   Store's own sequence of the event in the Task's log, gap-free from 0; `event_id` is the
   Store's ID of it and `event_type` its type (`TaskTransition@5`, `TaskExecutionStarted@1`,
   `TaskGateRetried@1` or `TaskProviderEvent@1`); `at_unix_ms` the policy time it records.
   `change` is the kind of what changed, from a closed list: the `kind` of a `TaskTransition@5`
   change (`lease_taken`, `execution_recorded`, `waiting`, `finished`, ...) or
   `execution_started`, `gate_retried`, `provider_event`. `detail` names the kind within an
   `execution_recorded` change (the execution record's `kind`: `invocation`, `reserved`,
   `started`, `settled`, `published`, ...), a `waiting` change (its reason) and a
   `provider_event` (its event's kind), and is null on every other. `node` and `attempt_id` name
   the node and Attempt the event concerns, read from the records it references; `result` is how
   a `settled` Attempt ended, and `charged_tokens` the exact decimal charge a `settled` or
   `usage_observed` record carries (ADR-0078). Every event the Store records is a line, lease
   renewals included: a follower that skips what it does not need still sees no gap.
3. **Exactly one terminal record, last.** `record` is `terminal`; it carries `last_sequence`,
   the sequence of the last event the stream covered, the Task's `state` (`phase`, `outcome`,
   `cause` and `next_action`, the same object every Task document carries, with the selectors the
   command was given) and `exit_code`. Its events and its state are read from one projection of
   the Task, so it never claims a state whose events it has not printed. It has no `sequence` of
   its own: the sequence numbers are the Store's, and the Task's log goes on when a settled Task
   is resumed.
4. **Resume by sequence.** `--after N` prints only the events after sequence N: a follower that
   reconnects with the last `sequence` it read gets the rest and nothing twice, and one that read
   the terminal record and reconnects with its `last_sequence` gets what the Task recorded since
   and a new terminal record. An N past the Task's last event is refused (exit 1), naming the
   last one. `--timeout S` stops after S seconds without a terminal record, exits 124 and names on
   stderr the exact `af task events TASK_ID --after N` that continues.
5. **No `--json`.** The stream is many documents, so the command takes no `--json`, and passing
   one is a usage error (exit 2). Every other command's `--json` stays exactly one document,
   unchanged. Every line of the stream's stdout is still a JSON document: a refusal is one
   `af/error@2` line, and a usage error is one too, with their usual exit codes.
6. **No second source of truth.** Nothing new is recorded: every line is derived from the Task's
   log and the artifacts its events reference, when the command runs.

## Considered options

- **`af task run --events FILE`.** Rejected: only the process that runs the Task would write the
  stream, so a Task started elsewhere, a detached run's parent or a second follower could not
  follow it, and the file would be a second record of what the Store already holds.
- **`af task wait ID --jsonl` or `af task watch ID --jsonl`.** Rejected: `wait` prints one
  status document under `--json`, and a flag that turns it into many would make one command
  print two shapes; a `watch` that needs a flag to print anything adds a text mode nobody asked
  for. A verb whose output is always the stream says what it does.
- **A sequence of the stream's own, the terminal record numbered after the last event.**
  Rejected: a resumed Task records more events after a terminal record, and a number the stream
  invented would collide with the Store's next one. The Store's sequence is already gap-free and
  stable across readers.
- **Carry each event's payload.** Rejected: payloads reference artifacts by ID, can be large, and
  change version with the Store; the line names what changed in af's own words, and `af task
  show --json` holds the rest.

## Consequences

- An agent follows a Task with `af task events TASK_ID`, branches on `change` and `detail`, and on
  the terminal record reads `state.outcome` and runs `state.next_action`, as with every other Task
  document.
- A follower's position is one integer it already holds, the last `sequence` it read.
- A new kind of Task change or execution record needs a new `change` or `detail` here; the
  exhaustive match in `review_core::task::agent_loop::TaskEventChangeV1::of` makes that a
  compile error, not a silent gap.

Tests: `schema_parity::task_agent_loop::event_stream_lines_are_closed` (both record shapes against
their schema, and broken or other-version lines refused by both);
`task_execution::events::tests::a_transition_is_one_line_with_the_stores_sequence_and_its_change`
and `the_terminal_exit_code_is_the_waits`; the integration test
`task_agent_loop::a_detached_run_is_followed_cancelled_and_resumed` (a live follower of a
detached run sees gap-free sequences and exactly one terminal record with the outcome and
`next_action` when the run is cancelled; `--timeout` stops with 124 and the `--after` that
continues; `--after N` returns only the rest; the resumed run's events continue the sequence);
and `cli_surface` (the help, the catalog entry, no `--json`, and a usage error as one
`af/error@2` line).
