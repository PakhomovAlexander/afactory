# Driving af from an agent

## Repository entry point

`af help agents --snippet` prints the following block for `AGENTS.md`. Preview its installation
with `af setup agents`; `af setup agents --write` installs the block and the two af skills.

<!-- af-agent-snippet:start -->
<!-- af-agent-help:start -->
For repository work with af, run `af help agents` first.
Read `af help json`, `af help exit-codes`, and `af help task-file` for machine contracts.
Start with `af provider status --json` and `af onboard` to inspect readiness.
Plan a Task with `af task plan --file TASK_FILE --json` before running it.
Follow `state.next_action` in each JSON result; do not guess a next command.
Use `af task findings TASK_ID --json` and `af task diff TASK_ID --json` for evidence.
Never add `--login` inside an agent session; ask the person to log in privately.
<!-- af-agent-help:end -->
<!-- af-agent-snippet:end -->

## Replace your scripts

| Script pattern | af command or setting |
|---|---|
| Read CAS findings | `af task findings TASK_ID --json` |
| Write a candidate | `af task materialize TASK_ID --into DIR` |
| Parse check failures | `af task show TASK_ID --checks --json` |
| Scrape a plan ID | `af task plan --file TASK_FILE --json` |
| Paste findings into a goal | `af task run NEW --from OLD` |
| Build a PR body | `af task report TASK_ID --pr-body` |
| Check disk before launch | `[storage] min_free_bytes` |
| Retry a failed Gate | `af task run TASK_ID --retry-gate` |

The findings command reads a recorded review round; an absent round appears in state.
Materialize requires a clean Git worktree unless `--force`; add `--allow-unverified` for an
unverified candidate or `--snapshot ID` for another recorded Snapshot. Show reads recorded checks,
including failures, without rerunning them. Plan is token-free; read `.plan_id` from its JSON.
Continuation uses OLD's reviewed candidate and recorded findings, then requires confirmation of
the exact new plan. The PR body requires the repository template and a valid lines block; failure
prints no partial body. The machine's free-disk floor refuses work before spend. Gate retry needs
an eligible failed check on the same sealed candidate and remains bound by approval, deadline and
budgets; it does not decide that a failure was flaky.

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

## Running af from inside an agent

An agent's host sandbox may deny a CLI probe, such as `codex app-server` before it answers
`initialize`. `af provider status --json` and `af provider detect --json` then report
`probe_denied` and exit 8: compatibility and login remain unknown. Tell the user to run af outside
that sandbox or approve the needed host permission, then retry the command. Reinstalling the CLI
does not grant a sandbox permission. `af doctor` shows the detected harness, sandbox and network
context; these markers explain the environment but never prove that a Provider CLI is compatible.

## 1. Plan

`af task plan --file TASK_FILE --json` captures the Task file and the checkout and compiles the
plan without running anything: one `af/task-inspection@13` document whose `plan_id` is the
plan to confirm and whose `state.next_action` is the command that runs it. A plan that does
not fit its limits is refused with `af/task-selection@3`, whose `next_action` is the `--fit`
command that plans with the smallest limits that fit.

## 2. Run, or run in the background

`af task run TASK_ID --confirm-plan PLAN_ID --json` runs exactly that plan and prints the
finished Task as `af/task-inspection@13`. While it waits it prints on stderr one line per node
that starts or finishes, with the time since the run started and the tokens charged so far
(`af task run: implement finished (2 min elapsed, 41k tokens so far)`), and every 5 minutes a
heartbeat (`af task run: implement running 23 min, 412k tokens, host load 11.2`); `--quiet`
keeps them back.

An agent that must not hold a terminal for the whole run detaches it:
`af task run TASK_ID --confirm-plan PLAN_ID --detach --json` returns once the run holds the
Task, with one `af/task-detached@2` document naming its pid, its host and its log, a file under
the Store's `runs/` directory, and `admitted: true`. A run not admitted within 2 minutes is not
claimed to run: exit 4, `admitted: false` and `next_action` `af task status TASK_ID`. The run
survives the shell that started it. Then
`af task wait TASK_ID --json` blocks until the Task settles and exits with its exit code
(`--timeout S` gives up with exit 124), and `af task status TASK_ID --json` answers at once;
both print `af/task-status@2`: the state, the node that runs, the time since the Task's clock
started, the tokens reserved, charged and remaining, and any unknown usage. When the Store has a
check span from the same repository and code policy for a check in this plan, status and plan show
its last recorded duration; with no such history they show no hint. `af task wait --bell` rings
only on terminal stderr at completion. `--notify` requests an OS notification without changing
the wait result if the notifier is unavailable. `af task cancel TASK_ID --json` stops the run and
its Worker processes, recording who asked, and prints `af/task-cancelled@1`; the Task stays
resumable with
`af task run TASK_ID`.

## 3. Read how it ended

Every document above carries `state`: `phase`, `outcome`, `cause` and `next_action`, the one
command to run next. The exit code says the same to a script: 0 verified, 3 changes requested,
4 exhausted or cancelled, 5 gate failed, 6 Provider failed, 7 expired (`af help exit-codes`).
stderr's last line is always `Task TASK_ID: phase/outcome/cause; next: COMMAND`. Branch on
`state.outcome` and run `state.next_action`; never parse the prose. `next_action` carries the
`--repo`, `--state` or `--workspace` the command was given, so it runs as printed. A refusal
under `--json` is one `af/error@2` document with the same exit code; one af classifies, such as a
node past its recovery allowance, also carries `cause`, `next_action` and `details`
(`af help json`).

## 4. Read the findings

`af task findings TASK_ID --json` prints the last review round as one `af/task-findings@1`
document: per reviewer, each finding with its severity, confidence, title, file and line, the
quoted evidence and the proposed fix, the round's counts by severity, and the evaluator's
verdict on that round with its reason. `--round N` reads an earlier round, with its own
verdict. A Task that recorded no review round has `round: null`, and its `state` says how far
it got. After `reviewer_findings` this is the command `next_action` names.

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

A Task that ended with changes requested is not run again: continue it as a new Task.
`af task run NEW_ID --from TASK_ID [--round N] --json` plans NEW_ID from TASK_ID's own Task
file. Its source is the candidate that round reviewed (the last round unless `--round` names
one), not the commit, and its implementer receives that round's reviewer findings and the
evaluator's verdict on the Pipeline's `findings` input, so nothing is pasted into the goal;
`--goal FILE` replaces the goal. It prints the plan for
`af task run NEW_ID --confirm-plan PLAN_ID --json`, or runs it at once with `--execute`, as in
step 2.

The low-level form binds the source by hand in a new Task file,
`"inputs": {"source": {"task": "TASK_ID", "port": "snapshot"}}`: it builds on the candidate but
carries no findings and no verdict. To check a candidate without changing it, the new Task is of
kind `verification` with that binding: its Pipeline verifies the tree read-only.

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
`scripts/pr-lines.py` prints, when it has one, counted from `--base REF` (origin/main by
default). When the script cannot print its block, nothing is printed and the command exits 1,
naming `--base REF`; a block it prints incomplete, without its end marker or its title, is
refused the same way, naming what is missing. `gh pr create --body-file body.md` takes it
unchanged. `af task report TASK_ID... --json` prints the same figures as one
`af/task-report@2` document.
