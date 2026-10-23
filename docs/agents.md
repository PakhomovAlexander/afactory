# Driving af from an agent

This is the loop a coding agent, or a script, runs to get a change made through af: plan a
Task, run it, read how it ended, read its findings, look at its change, run another round
when it needs one, deliver it, and describe it in a pull request. Every step is one `af task`
command; with `--json` each prints exactly one versioned document on stdout, and progress and
the last line go to stderr (`af help json`). `af help agents` prints this page.

A Task is one bounded unit of work: a goal, its limits, and the Pipeline that does it. A
Worker is a program a Pipeline node runs (a model CLI or a command), and each try of one is an
Attempt. A Snapshot is an exact tree of files the Task recorded: its source, and the candidate
its implementer produced. The Store is the directory that keeps all of it (`--state`, or a
default per checkout). `af help task-states` lists every state a Task can be in.

## 1. Plan

`af task plan --file TASK_FILE --json` captures the Task file and the checkout and compiles the
plan without running anything: one `af/task-inspection@13` document whose `plan_id` is the
plan to confirm and whose `state.next_action` is the command that runs it. A plan that does
not fit its limits is refused with `af/task-selection@2`; `--fit` plans with the smallest
limits that fit.

## 2. Run, or run in the background

`af task run TASK_ID --confirm-plan PLAN_ID --json` runs exactly that plan and prints the
finished Task as `af/task-inspection@13`. While it waits it prints on stderr one line per node
that starts or finishes, with the time since the run started and the tokens charged so far
(`af task run: implement finished (2 min elapsed, 41k tokens so far)`), and every 5 minutes a
heartbeat (`af task run: implement running 23 min, 412k tokens, host load 11.2`); `--quiet`
keeps them back.

An agent that must not hold a terminal for the whole run detaches it:
`af task run TASK_ID --confirm-plan PLAN_ID --detach --json` returns once the run holds the
Task, with one `af/task-detached@1` document naming its pid, its host and its log, a file under
the Store's `runs/` directory. The run survives the shell that started it. Then
`af task wait TASK_ID --json` blocks until the Task settles and exits with its exit code
(`--timeout S` gives up with exit 124), and `af task status TASK_ID --json` answers at once;
both print `af/task-status@1`: the state, the node that runs, the time since the Task's clock
started and the tokens so far. `af task cancel TASK_ID --json` stops the run and its Worker
processes, recording who asked, and prints `af/task-cancelled@1`; the Task stays resumable with
`af task run TASK_ID`.

## 3. Read how it ended

Every document above carries `state`: `phase`, `outcome`, `cause` and `next_action`, the one
command to run next. The exit code says the same to a script: 0 verified, 3 changes requested,
4 exhausted or cancelled, 5 gate failed, 6 Provider failed, 7 expired (`af help exit-codes`).
stderr's last line is always `Task TASK_ID: phase/outcome/cause; next: COMMAND`. Branch on
`state.outcome` and run `state.next_action`; never parse the prose.

## 4. Read the findings

`af task findings TASK_ID --json` prints the last review round as one `af/task-findings@1`
document: per reviewer, each finding with its severity, confidence, title, file and line, the
quoted evidence and the proposed fix, the round's counts by severity, and the evaluator's
verdict with its reason. `--round N` reads an earlier round. A Task that recorded no review
round has `round: null`, and its `state` says how far it got. After `reviewer_findings` this
is the command `next_action` names.

## 5. Look at the change

`af task diff TASK_ID --json` prints the candidate's change against the Task's source as one
`af/task-diff@1` document, the files with their status and line counts and the unified diff
`git diff` would print, without touching any worktree; without `--json` it prints the diff
itself, `--stat` or `--name-only` its summary.

`af task materialize TASK_ID --into DIR --json` makes the tracked tree of the Git worktree DIR
equal to the candidate, writes, deletions and executable bits, and prints one
`af/task-materialized@1` document with the paths it wrote, deleted and changed in mode. It
refuses a DIR with uncommitted changes unless `--force`, and a candidate the Task did not verify
unless `--allow-unverified`, which then says `unverified candidate: review it before merging`.
Nothing is committed or staged: `git -C DIR status` shows the change.

## 6. Run another round

A Task that ended with changes requested is not run again: plan a new Task, with a new
`task_id`, whose Task file names the goal that is left. To build on the reviewed candidate
rather than on the commit, bind its source to that candidate,
`"inputs": {"source": {"task": "TASK_ID", "port": "snapshot"}}`, then plan and run it as in
steps 1 and 2. To check a candidate without changing it, the new Task is of kind
`verification` with the same binding: its Pipeline verifies the tree read-only.

## 7. Deliver

`af task deliver TASK_ID --branch af/TASK_ID --worktree ../TASK_ID --confirm TASK_ID --json`
places a verified Task's Snapshot in a new local branch and worktree and prints its
`af/task-delivery@1` receipt; it never commits, pushes or opens a pull request. A verified
verification Task of another Task's candidate delivers that candidate on the other Task's
committed source, and the delivery records both Tasks, so a candidate verified read-only is
delivered without being made again. A candidate no Task verified is refused here; step 5's
`--allow-unverified` writes it where a person can look at it.

## 8. Describe it in a pull request

`af task report TASK_ID... --pr-body > body.md` prints the whole pull request description the
repository's template asks for: the template with the `af task report` block between its
`<!-- af-task-report:v1 -->` markers, and the lines added block that the checkout's
`scripts/pr-lines.py` prints, when it has one. `gh pr create --body-file body.md` takes it
unchanged. `af task report TASK_ID... --json` prints the same figures as one `af/task-report@2`
document.
