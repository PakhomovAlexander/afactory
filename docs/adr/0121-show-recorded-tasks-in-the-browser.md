# ADR-0121: Show recorded Tasks in the browser from their inspection documents

Status: accepted, 2026-09-24.

## Context

[`docs/design/tui.md`](../design/tui.md) §5.5 plans the browser's Tasks pane, and §7 step 3
delivers it as package M3. The pane lists the scope's Tasks by state. For one Task it shows its
identity, one row per plan stage with attempts, time and tokens, the token and time totals, and
its event history. While the Task runs, the pane reads it again every second.
[ADR-0119](0119-open-a-read-first-browser-on-bare-af.md) binds the browser as a projection. A
number no CLI document carries is not shown, and a pane never writes a Store.

Five facts shape the answer:

- `af task show --json` prints `af/task-inspection@11` and `af task list --json` prints
  `af/task-list@2`. The show document carries the history, the execution records, the run
  reports, the Attempt walls, the runtime observations and the Task's chargeable total. It does
  not carry the plan's graph order, the goal, or the node that an execution record ran.
- `af task explain --json` is the show document plus the captured `plan` and its `graph`. That
  gives the graph order, each node's Attempt allowance and the root Pipeline's name.
- Execution records name an Attempt's node only through artifacts. A `reserved` record names its
  invocation, and a `published` record without an Attempt names its output. The invocation
  artifact holds the node, and the output artifact holds its invocation.
- §5.5 names `TaskCompleted@1` totals as the TOKENS source. No such event exists. The only
  per-Attempt token components in the document are in `attempt_walls`. The document carries
  those walls only for Attempts that recorded runtime evidence.
- `af task` without `--state` keeps each repository's Store at
  `$XDG_STATE_HOME/af/task/local/<16 hex of the canonical repository path>`. The directory records
  no repository path.

## Options

- **Add the missing fields to the inspection document**: an Attempt's node, per-Task token
  components and the goal. This was rejected because it changes a published `--json` document for
  a display concern, and the package must leave every document byte-identical.
- **Query the Store directly from the pane**, projecting execution state the way the scheduler
  does. This was rejected because the pane would then have a second reading of the records, which
  could disagree with the CLI's.
- **Read the documents, and resolve the artifacts they name by ID.** The same CAS lookup shows an
  artifact when `Enter` is pressed on a HISTORY row. This was chosen.

## Decision

### Reading

The document builder behind `af task show` and `af task explain` is split from its printer.
`present_with_format` prints what `task_execution::inspection` builds, so every `--json` and text
output stays the same. The pane reads the following, and writes nothing:

- `task_execution::list_common`, for the list;
- `task_execution::inspection_document(state, id, true)`, the `af task explain --json`
  document, for one Task;
- `task_execution::recorded_artifact`, a read-only CAS lookup, for three things only:
  - the revision that `revision_id` names, for the kind and goal;
  - the invocation or output artifacts that the execution records name, for each record's node;
  - the artifact of a HISTORY row that `Enter` opens.

`task_execution::default_task_state` is now the one spelling of the default `--state`
directory. `af task` and the pane both use it. The project scope reads that directory for its
repository. The user scope reads every directory under `local_task_states` and lists each one
under its opaque name, because nothing in the Store names the repository. A Store the binary
cannot read becomes a `! Store unreadable` bar row, and the folder pane names the directory and
the refusal. It is never shown as an empty list, and the browser does not crash.

### Grouping and progress

- A Task whose phase is `ready`, or `waiting` on `needs_plan_review`, is **awaiting approval**.
  Any other unfinished phase is **running**.
- A finished Task is **done** when its result's acceptance is `satisfied`, and **failed**
  otherwise.
- Empty groups are not listed. Tasks are newest first, by the time of their first event.

A stage is one node of `graph.order`. Its mark is taken from the execution records and from the
last whole-Round run report of the current plan:

- `[..]`: an Attempt is reserved and not settled.
- `[!!]`: the report failed the node, or its last Attempt settled failed.
- `[ok]`: an output was published, an Attempt succeeded, or the report completed the node.
- `[  ]`: the report suppressed the node (the row also says "skipped"), or the node was not
  reached.

Progress is the settled stages (ok, failed or suppressed) over the length of `graph.order`. A
stage's wall is the sum of its `attempt_walls`, and its tokens are the charges its settled
Attempts recorded. Each is shown only when the records carry it.

### Totals

- TOKENS takes `chargeable` from the document's `chargeable_tokens`. It shows a component (input,
  output, cache read, reasoning) only when `attempt_walls` covers every settled Attempt and each
  wall carries that component. Otherwise the component is `-`, because a partial sum is not the
  Task's total.
- TIME takes the wall from the first and last event times. For an unfinished Task the wall runs
  to now. Checks and dependency preparation are the sums of the runtime spans of those kinds.
  §5.5's "verification" time is not shown, because no record carries it.

### Live reads and keys

A running opened Task is read again on a thread about once a second, and the event loop collects
the result. A new `Pane::close` stops those reads when the main pane shows another pane. A
finished Task is never read again. `R` reads at once.

The Tasks pane uses these keys:

- `Enter` on a HISTORY row puts the artifact's pretty JSON over the pane. `Pane::nested` and
  `Pane::back` let `q` and `Esc` close that view and return to the row.
- `p` sends the new `Effect::OpenPipeline`. The browser then opens the Pipelines pane's entry of
  that name, or says on the status line that the pane does not list it.
- `y` yanks the Task id, from both the bar and the pane.

The bar gains folding groups (`NodeKind::Group`), which keep their folds across a re-read.

## Consequences

The pane cannot disagree with `af task show --json`. A pseudo-terminal test drives bare `af` over
a Task that `af task start --execute` recorded, and compares every number on the screen with that
document's fields. Goldens at 100x30 pin the bar and one Task's pane, taken from Tasks run
token-free through the fixture's command Workers. Only artifact IDs, times and durations are
masked.

- A stage's wall stays blank for an Attempt that recorded no runtime evidence. The TOKENS
  components stay `-` until every settled Attempt carries a wall with them.
- `p` shows the Pipelines pane's plan of the committed package, not the captured plan.
  `af task explain` remains the view of the captured plan.
- No wire contract, schema, fixture or `--json` document changes. `CONTEXT.md` gains no term.
