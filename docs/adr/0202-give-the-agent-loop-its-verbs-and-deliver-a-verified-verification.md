# ADR-0202: Give the agent loop its verbs, and deliver a verified verification

**Status:** accepted (2026-10-23); amended 2026-10-24
([a detached run is admitted or says it is not](#amendment-2026-10-24-a-detached-run-is-admitted-or-says-it-is-not)).
Narrows
[ADR-0031](0031-deliver-verified-tasks-to-new-local-worktrees.md) (delivery accepts only a
verified Task whose Snapshot descends exactly from its source) by one named exception, and
extends [ADR-0129](0129-stop-task-workers-when-af-is-interrupted.md) (an
interrupt stops the Workers and leaves the Task resumable) to a cancel from another process.
Keeps [ADR-0142](0142-carry-the-af-task-report-in-every-pull-request.md) and
[ADR-0195](0195-state-the-lines-a-pull-request-adds-per-directory-and-check-them.md) (the two
blocks a pull request description carries) and
[ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md) (every new
shape is versioned; another version fails closed).

## Context

Every campaign that drove af from a shell or an agent rebuilt the same verbs around it: a script
that walked a Task's review rounds into the CAS to print the findings and the evaluator's
verdict; one that made a worktree's tracked tree equal to a Store Snapshot, because `af task
deliver` refuses an unverified candidate and a re-rooted source, and a reviewed but unverified
candidate is exactly what a person wants to look at or commit; a splice of `af task report`
into the pull request template; a collector that adopted an orphaned `af task run` by its pid;
cancel sentinel files and process-group kills; and an orchestrator that strung all of it
together. There was no way to see a finished Task's change without delivering it. A candidate
that a second, read-only verification Task verified could not be delivered either: the
verification Task's Snapshot is its source, so it does not descend from it, and its source is a
re-rooted tree with no commit; the candidate was paid for a third time to be delivered.

## Decision

The loop's verbs are `af task` commands, each with a versioned `--json` document and a schema
under `schemas/`.

- **`af task findings ID [--round N]`** prints a recorded review round, the last by default: per
  reviewer, each finding with its severity, confidence, title, location, quoted evidence and
  proposed fix, grouped by severity in text, then the evaluator's verdict and its reason
  (`af/task-findings@1`). A complete round reads its Finding Set; an incomplete one shows each
  reviewer's reports as recorded, never as gathered evidence (ADR-0034). A Task without a round
  says so with its state. It is the command `reviewer_findings` names as next.
- **`af task diff ID [--snapshot SHA] [--base SHA] [--stat | --name-only]`** prints the change from
  the Task's source to its candidate (its result's Snapshot, else the last one a node sealed),
  or between two Snapshots the Task recorded, as `git diff` prints it for the same two trees
  (`af/task-diff@1`). It writes the entries the two differ in into a scratch Git object database
  and runs `git diff-tree -M` there, under the isolated Git environment delivery uses; no
  worktree and no Store is touched.
- **`af task materialize ID [--snapshot SHA] --into DIR [--allow-unverified] [--force]`** makes
  the tracked tree of the Git worktree DIR equal to the candidate: it reads DIR's tracked files
  as they are on disk, and applies the manifest difference to the candidate with the no-follow
  removal and materialization the warm workspaces use, then reads every written path back
  (`af/task-materialized@1`). It refuses a DIR with uncommitted changes unless `--force`, and a
  candidate that is not the Task's verified result unless `--allow-unverified`, which then says
  `unverified candidate: review it before merging`. It never commits or stages. The `git
  status` it runs before it writes is the one a person runs in that worktree; nothing runs in DIR
  after the candidate's files are in it.
- **Run records.** Every `af task run` and `af task start --execute` writes
  `runs/<task>-<n>.json` (`af/task-run-process@1`) in the Store before it takes the Task's lease:
  its pid, host, the lease's writer, and for a detached run its log. `runs` is an af entry of the
  Store directory; collecting a Task removes its run files.
- **`af task run ID --detach`** starts the same command again in the background, its stdin empty,
  its output appended to `runs/<task>-<n>.log`, in its own process group and with the terminal's
  hangup blocked, so it outlives the shell that started it; it returns once that run holds the
  Task and the plan is admitted, with the pid and the log (`af/task-detached@1`). The detached
  run is an ordinary `af task run`, told only its run number (`AF_TASK_RUN_DETACHED`, which af
  sets). No power assertion is held, by a foreground run or a detached one.
- **`af task wait ID [--timeout S]`** blocks until the Task settles — no lease is live and no
  recorded run of this host is starting — then exits with the Task's code (4 for cancelled or a
  person's decision), or 124 after `--timeout`. **`af task status ID`** prints one line at once:
  the state, the node whose Attempt runs, the time since the Task's clock started, the tokens so
  far, the run and who cancelled it (`af/task-status@1`).
- **`af task cancel ID`** finds the run by the Task's lease: the run record whose writer is the
  lease's. It refuses a record of another host, naming its pid and host, and a Task no writer
  holds. It writes `runs/<task>-<n>.cancel.json` (`af/task-cancel-request@1`) with the lease's
  epoch and the requester, then sends SIGTERM to that one pid, and once more after 60 s; the run
  stops its Workers as on Ctrl-C (ADR-0129), releases the lease and exits. No process is ever
  found by name or arguments. The Task's state derivation reads the request: a Task that stopped
  under the very lease epoch a request names is `abandoned/cancelled/host_cancelled`, whatever
  its last Attempt was doing, and `af task show` names the requester. It stays resumable.
- **Progress.** While `af task run` waits, a watcher on its own read-only Store connection
  prints on stderr one line per node that starts or finishes, with the time since the run
  started and the tokens charged so far, and every five minutes a heartbeat with the running
  node, the tokens and the host's load; `--quiet` keeps them back. stderr's last line is the
  state and the next command in text as under `--json`.
- **`af task report IDS… --pr-body [--template FILE]`** prints the description the template asks
  for (`.github/PULL_REQUEST_TEMPLATE.md` of `--repo` by default): the report between its
  `<!-- af-task-report:v1 -->` markers, and the block the checkout's own `scripts/pr-lines.py`
  prints between its `<!-- af-pr-lines:v1 -->` markers when it has the script. A script that
  fails is said on stderr and its block left as the template has it.
- **`af help agents`** is `docs/agents.md`, included byte for byte.

Delivery keeps its rule with one exception. A verified Task of kind `verification` whose
Snapshot equals its source, and whose source it bound from another Task's recorded output
(`inputs.source = {task, port}`), proves that candidate. `af task deliver` then checks the
candidate Task as its own delivery would: its result is still the one bound, the bound Snapshot
is that result's output and descends exactly from the candidate Task's root source, and that
source is a commit. It delivers the candidate's tree on that commit, under the verification
Task's lease, and every delivery record references one `af/TaskDeliveryProof@1` naming both
Tasks and their results; the Store refuses a proof that names another Task. The receipts keep
their shapes. Every other Task's refusal is unchanged, and it now names `af task materialize`.

## Consequences

- An agent drives the loop with `af` alone; every step has an exact command and a versioned
  document, and `docs/agents.md` is the walk-through.
- A candidate is looked at without a delivery, and a reviewed but unverified one is written into
  a worktree only by an explicit flag; delivery still never places an unverified tree.
- A verified read-only verification is the proof delivery needs, so a candidate is implemented
  once.
- Cancellation needs a run record: a Task run by an earlier release, or from a Store shared with
  another host, is refused with what af knows, and is stopped where it runs.
- A progress watcher reads the Store while the run writes it; it re-projects only when the log
  grew, and polls less often when projection is slow.

## Amendment, 2026-10-24: a detached run is admitted or says it is not

`af task run --detach` waited two minutes for its run to take the Task and admit its plan, and
then returned exit 0 with `af/task-detached@1` although the run had done neither, so a script
was told the Task ran when it might never start. The document is now `af/task-detached@2`
(`schemas/task-detached-v2.json`); version 1 fails closed. It adds `admitted`, true only once
the run holds the Task's lease and its plan is admitted, and `next_action`: `af task wait ID`
for an admitted run, `af task status ID` for one still starting, and the Task's own next command
for a run that ended before it went to the background. A run not admitted within the window is
not claimed to run: the command exits 4 with `admitted: false`. The rest of the decision stands.
The same close-out keeps its verbs exact: a second `af task cancel` of a run reports the first
request, which stands, rather than its own; `af task status` names the run whose writer holds the
lease and puts the next command on its own lines within 100 columns; `af task run` prints a
`started` line for every node it runs, the kernel's own nodes included, before its `finished` or
`failed` line; and `--pr-body` exits 1, naming `--base REF`, when the checkout's
`scripts/pr-lines.py` cannot print its block, instead of printing a body the pull request check
refuses.
