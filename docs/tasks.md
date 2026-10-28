# Implementation Tasks with `af task`

`af task` runs one bounded implementation Task against a local Git repository and, when the
result is verified, delivers it to a new local branch and worktree for a human to inspect and
commit. A Task starts from a Task file that names its ID, kind and goal; limits may come from
the project defaults. The Pipeline and
Worker packages that run it are pinned in the repository's committed Task catalog. This page
covers the `implement` Task and its delivery. Embedded Review, generated plans, repair and the
other Task kinds are described under [Task execution](task-execution.md).

## Supported boundary

An implement Task runs its captured Pipeline over a captured source Snapshot. An implementation
Pipeline such as the starter `builtin/implementation-small` runs:

1. an implementer Worker edits a private sandbox, and the result is sealed as a derived Snapshot;
2. the required read-only checks of the repository's code policy inspect that Snapshot;
3. an independent evaluator verifies it against the Task's requirements; it receives only its
   declared inputs, never the implementer's transcript;
4. a verified result remains an immutable Snapshot until an operator explicitly delivers it;
5. delivery creates a new local branch and linked worktree containing those exact bytes as
   uncommitted work.

Delivery never changes the source checkout, commits, pushes, opens a pull request, invokes a
remote, or receives repository credentials
([ADR-0031](adr/0031-deliver-verified-tasks-to-new-local-worktrees.md)). Hosted execution and
automatic Integration are outside this path.

## Prerequisites

- a local Git clone with no staged, tracked or untracked changes at the commit used to start
  the Task;
- an installed `af` release whose archive digest and `SHA256SUMS.minisig` signature were both
  verified. The release's `install.sh` always checks the digest against `SHA256SUMS`, and the
  signature with `minisign`: put it on `PATH` before you run the installer
  ([install](../README.md#install)), which refuses without it. Under
  `AF_INSTALL_ALLOW_UNSIGNED=1` it proceeds anyway, reports that only the checksum is verified,
  and the install receipt records `verified_by = "sha256sums"` rather than `"minisign"`; that
  checksum-only install does not meet this prerequisite. The running release's
  `receipt.verified_by` is shown by `af self status --json`;
- machine-local Provider authentication for every model Worker the Pipeline binds, checked with
  `af provider status` and established as [`providers.md`](providers.md) describes — an
  interactive login is opt-in and happens only at your own terminal; command Workers need none;
- every tool named by the code policy's checks, such as the repository's compiler and test
  runner;
- a reviewed Task catalog committed in the repository: `.af/task-catalog.toml`, the Pipeline and
  Worker packages it pins, and `.af/code-policy.toml` with the required checks. Run
  `af onboard --tasks` to preview the generic model-driven catalog and the exact
  `[task.defaults]` text. `af onboard --tasks --apply` writes it with review authority
  in a new repository, or adds absent Task files and appends the table to an existing
  `.af/`. It refuses an existing target or `[task]` table without writing. Review the
  diff and commit `.af/` before planning; local, uncommitted files are not authority.
  With both Provider kinds registered, the generated implementer uses Claude and the
  reviewers use Codex; with one kind, distinct models under that login serve all Workers.

`af catalog init --profile software --destination task-demo` creates a new starter directory,
which must not exist yet, holding a working, credential-free catalog and runnable Task files
([starters](task-execution/starters.md)); it does not add a catalog to the current repository.
Its `--destination`, like that of `af catalog sync`, is a safe path relative to `--repo`
(default `.`); an absolute path is refused.
`af catalog sync` imports shared definitions from Git
([shared catalogs](task-execution/shared-catalogs.md)). The catalog pins every package by digest,
so changing any package byte requires a newly reviewed pin; `af catalog digest PACKAGE_DIR`
prints that digest ([pin a package](task-execution/shared-catalogs.md#pin-a-package)). Checks
are project-specific. The onboarded code policy uses the same discovered Gate as review
onboarding, or an explicit `--gate NAME=COMMAND`; inspect that check before committing.

Trusting that committed authority and running `af task start --execute`, or confirming the
previewed plan, authorizes Afactory to deliver each Worker its exact declared inputs for every
stage of the Task
([ADR-0033](adr/0033-configured-workers-authorize-declared-input-delivery.md)). There is no
additional per-Worker or per-call consent prompt. Delivery, Provider rebinding and any other
remote side effect remain separate explicit operations.

## Write a Task file

A Task file is JSON or TOML in the [`af.task-file/1`](../schemas/task-file-v1.json) shape. The
checked-in `fixtures/task-runtime/pagination` example uses this one:

```json
{
  "schema": "af.task-file/1",
  "task_id": "pagination-cli",
  "kind": "implement",
  "goal": "Implement this Jira ticket: offset/limit pagination",
  "pipeline": { "name": "fixture/implementation", "fallback": "refuse" },
  "strategy": "small",
  "facts": {},
  "limits": {
    "tokens": 1000,
    "max_attempts": 3,
    "wall_ms": 60000,
    "verification": { "tokens": 200, "attempts": 2, "wall_ms": 10000 }
  }
}
```

The Task ID is chosen here: planning the same file again shows the existing plan, and a changed
file under a planned ID needs `af task plan --replace`. A file that omits `strategy` takes its
kind's: `implement` is `reviewed` with `verification` `review`, `verification` is `light` with
`evaluation`, both with `facts.standard = true`, and `report` is `report`; a field the file names
overrides the kind's, and one its kind's profile cannot verify by is refused with both values and
this table. Without `pipeline`, planning
[selects](task-execution/selection.md) a fitting Pipeline from the catalog. The goal and any
`requirements` are input data; they cannot change the committed execution permissions. The
[Task-file walkthrough](task-execution/task-file.md) runs this example end to end.

### Short goals, context and project defaults

Only `task_id`, `kind` and `goal` are required; `schema` defaults to `af.task-file/1`
([ADR-0212](adr/0212-continue-a-task-from-its-reviewed-candidate-and-give-judging-workers-the-base.md),
`af help task-file`). What a Task file omits comes from `[task.defaults]` of `.af/af.toml` in the
committed Authority Snapshot: `limits`, `pipeline` and its `fallback`. A field the Task file names
wins whole, and the default Pipeline applies only to a Task of a kind it accepts. `af task plan`
prints on stderr what came from the defaults; without `limits` in either place the plan is
refused, naming both.

```toml
[task.defaults]
pipeline = "kernel/implementation-reviewed"
fallback = "refuse"

[task.defaults.limits]
tokens = 6000000
max_attempts = 16
wall_ms = 28800000

[task.defaults.limits.verification]
tokens = 2000000
attempts = 8
wall_ms = 14400000
```

The wall bounds leave room for the compiled reviewers, evaluator and repository checks of
`kernel/implementation-reviewed`; plan another Pipeline against its own declared allowances.

A goal can stay short and point at what it needs. `context` names files of the Task's source
tree, each with an optional `#section`; standing rules live in `.af/rules.md`. The Worker's
requirements end with a "Read first" list that names the Snapshot the Worker reads, then the rules
and each context file of that tree with the digest of its bytes there, recorded as one
`af/TaskReadFirst@2` artifact in the revision's provenance
([ADR-0254](adr/0254-name-the-tree-of-a-workers-rules-and-deliver-the-diff-subject-as-a-typed-input.md)).
A changed file is a changed plan, and a file the tree does not hold is refused by name. A
continued Task reads its candidate, so it lists the candidate's rules; its plan names that
Snapshot on stderr and says when the rules differ from what the continued Task read.

```json
{
  "task_id": "pagination-docs",
  "kind": "implement",
  "goal": "Document paginate; the design says how.",
  "context": ["docs/design.md#paginate", "pagination.py"]
}
```

### Structured acceptance

`acceptance` states, as typed items rather than prose in the goal, what the result must meet:
1 to 32 items, each an `id` (a name, once) and its `text`
([ADR-0255](adr/0255-say-a-check-stopped-at-its-wall-and-carry-typed-acceptance-in-the-check-receipt.md),
[schema](../schemas/task-acceptance-v1.json)). The list is captured at plan time as one
`af/TaskAcceptance@1` record in the revision's provenance, so a changed list is a changed plan.
The check node carries the items verbatim in its `af/TaskCheckReceipt@2`, and the selected
Pipeline hands the record to its evaluator on its optional `acceptance` root port. A Pipeline
that does not declare that port, or a kind whose accepting Pipelines do not all declare it, is
refused at plan time, before anything is recorded: its evaluator could never judge the items. So
is a Pipeline that declares the port but hands it to no `verify` evaluator node as that node's
own input, and a Task whose kind runs under a domain that does not judge items by ID (a
document, report or optimization Task): only an `implement` Task's evaluator is held to them. An
evaluation that passes the Task names every item ID as a word of its `reason`, `bounds: met`; one
that leaves an item out is refused as a reply that broke its output contract, and the evaluator
answers again or the Task does not verify. A Task file without `acceptance` plans and runs
exactly as before, with the same revision, plan and receipt contents.

```json
{
  "task_id": "pagination-bounds",
  "kind": "implement",
  "goal": "Refuse negative offsets and limits in paginate.",
  "acceptance": [
    {"id": "bounds", "text": "paginate refuses a negative offset or limit with ValueError"},
    {"id": "past-end", "text": "an offset past the end returns an empty page"}
  ]
}
```

## Start and inspect a Task

Run from the clean repository at committed `HEAD`. State defaults to a directory outside the
repository under the user's XDG state directory; `--state` selects another external directory,
which must be absent, empty or already an af Store: it becomes af-owned, so a directory holding
anything else is refused. `--workspace LABEL` selects a Workspace's Store and bindings file
([Projects and Workspaces](project.md)).
Save the Task request outside the checkout too (for example, `../ticket.json`, or a Workspace's
`tasks/` directory); Afactory captures its contents into Task state. `--file` selects the request,
not the source repository, so remain in the source checkout or select it with `--repo`.

```sh
af provider status
af task start --file ../ticket.json
# Review the preview and copy its complete Plan identity.
af task run pagination-cli --confirm-plan PLAN_ID
af task list
af task show pagination-cli
```

Before the plan, `af task plan` prints one line per Model Worker on stderr, `model WORKER MODEL
EFFORT` followed by `ok` (the Provider's catalog lists the exact ID and the effort level),
`unverified against the catalog` (a custom ID that passes the spelling rule, or a catalog the
harness could not give) or `refused:` and the hint, which refuses the plan. The catalog is read
token-free, once per Provider; a binding to an alias or an effort level the Provider does not
accept is refused before any Provider process starts. `af provider models <id>` lists the
spellings a Provider accepts ([model bindings](task-execution/model-bindings.md),
[ADR-0148](adr/0148-discover-provider-models-and-validate-every-binding-before-a-charge.md)).

The [ASCII preview guide](task-execution/preview.md) covers the two plan views and agent
approval. `task list` prints a header row and one line per Task: its ID, its state, its review
round, chargeable tokens, how long ago it last changed and the verb of its next command, then its
delivery once there is one. `task list --attention` groups the Tasks by what they wait for,
each with its whole next command ([Which Tasks need you](#which-tasks-need-you)). `task show`
adds the plan, the checks, each failed node and the next command; `--json` adds exact Snapshot
IDs and the append-only event history. Both accept `--json` ([Task states](#task-states)).

## Follow a run

While `af task run` waits it prints on stderr one line per node that starts or finishes, with the
time since the run started and the tokens charged so far (`af task run: implement finished (2 min
elapsed, 41k tokens so far)`), and every 5 minutes a heartbeat (`af task run: implement running 23
min, 412k tokens, host load 11.2`); `--quiet` keeps them back. stderr's last line is always the
Task's state and next command, `Task ID: phase/outcome/cause; next: COMMAND`, in text as under
`--json`.

`af task run ID --detach` runs the same command in the background and returns once that run holds
the Task and its plan is admitted, printing its pid, its host and its log (`af/task-detached@3`
under `--json`, `admitted: true`). A run that is not admitted within 2 minutes is not claimed to
run: the command exits 4 with `admitted: false` and `af task status ID` as the next command. The
run has no terminal, survives the shell that started it, and writes its
output to `runs/ID-N.log` in the Task Store; every run, in a terminal or not, also records its
process there as `runs/ID-N.json` before it takes the Task
([ADR-0211](adr/0211-give-the-agent-loop-its-verbs-and-deliver-a-verified-verification.md)).

```sh
af task run pagination-cli --confirm-plan PLAN_ID --detach
af task status pagination-cli
af task wait pagination-cli --timeout 3600
af task events pagination-cli --after 41
af task cancel pagination-cli
```

`af task status ID` prints at once the state, the node whose Attempt runs, the time since the
Task's clock started, the tokens reserved, charged and remaining (with unknown usage marked), the
last comparable recorded Gate duration when available, and the run that holds the Task's lease.
It puts the next command on its own line, broken between its arguments to fit 100 columns.
`af task wait ID` blocks
until the Task settles and exits with its exit code, or with 124 when `--timeout S` passes first;
both print `af/task-status@2` under `--json`. A comparable Gate uses the same check name, captured
code policy and source repository; no recorded check span means no hint. `af task wait --bell`
rings terminal stderr on completion. `--notify` tries the OS notifier and warns if unavailable
while preserving the Task exit code. `af task cancel ID` stops the `af task run` the Task's
writer lease names: it finds the process by the lease's writer in the run records, refuses one of
another host with its pid and host, records who asked in `runs/ID-N.cancel.json`, and signals that
recorded pid only, which stops its Worker processes as Ctrl-C does. The Task is then
`abandoned/cancelled/host_cancelled`, `af task show` names who cancelled it, and `af task run ID`
resumes it. `af task events ID` follows the Task as JSON lines, one `af/task-event@1` document per
event of its log with the Store's own gap-free `sequence`, and once the Task settles one terminal
record with its state and exit code; `--after N` prints only the events after sequence N, so a
follower that reconnects gets the rest, and the command takes no `--json`
([ADR-0250](adr/0250-follow-a-task-as-a-stream-of-its-store-events.md)).
[docs/agents.md](agents.md) walks the whole loop as an agent runs it.

## Task states

Every `af task` command that shows a Task prints one state for it, in text and under `--json`
alike: `af task list`, `af task show`, `af task report` and the last line of `af task run`. The
state is `phase/outcome/cause`, for example `finished/gate_failed/check_failed`; `outcome` is
present once the Task ended and `cause` once that outcome is not `verified`. Under `--json` it is
the `state` object: `phase`, `outcome`, `cause` and `next_action`, `null` where absent
(`af help task-states` prints the same tables).

```text
  planned --af task run--> running --> finished
     |                      |   ^
     |                      v   | af task run (resume)
     |                   abandoned
     +--> waiting (plan review, resources, input or a decision) --> running
```

| Phase | Meaning |
| --- | --- |
| `planned` | a plan is recorded and waits for `af task run` to confirm it |
| `running` | a writer holds the Task's lease and runs it now |
| `waiting` | the Task waits for a plan review, resources, input or a decision |
| `abandoned` | the Task stopped before it finished and no writer holds it |
| `finished` | the Task recorded its result (or was collected after it did) |

| Outcome | Exit code of `af task run` | Meaning |
| --- | ---: | --- |
| `verified` | 0 | every acceptance obligation is satisfied; the result can be delivered |
| `changes_requested` | 3 | the review or the evaluator ran to completion and asked for changes |
| `gate_failed` | 5 | a required check of the gate failed or could not run |
| `provider_failed` | 6 | a Worker's Provider or process failed, or its reply was not admissible |
| `cancelled` | 4 | the run stopped before it finished; `af task run` resumes it |
| `exhausted` | 4 | a token, Attempt or verification-reserve limit ran out |
| `expired` | 7 | the Task's wall clock ran out |

| Cause | Outcome | Meaning |
| --- | --- | --- |
| `reviewer_findings` | `changes_requested` | the review recorded findings at or above its gate |
| `evaluator_rejected` | `changes_requested` | the independent evaluator did not accept the result |
| `check_failed` | `gate_failed` | a required check ran and failed, or the kernel stopped it at its wall |
| `check_flaky_then_failed` | `gate_failed` | a required check failed; some of its tests were flaky before |
| `preflight_failed` | `gate_failed` | a required check stopped at a step before its tests, or never started |
| `malformed_reply` | `provider_failed` | a Worker's process failed or its reply broke its output contract |
| `provider_network` | `provider_failed` | a request to the Provider failed on the network |
| `provider_capacity` | `provider_failed` | the Provider's model was at capacity or its rate limit was reached |
| `provider_timeout` | `provider_failed` | a Worker or Provider call ran out of its Attempt time |
| `provider_environment` | `provider_failed` | the Provider's environment on this machine failed, not the model or the login |
| `identity_changed` | `provider_failed` | the Provider answered as another account, model or policy than the plan bound |
| `identity_unverified` | `provider_failed` | the Provider's login could not be verified |
| `model_refused` | `provider_failed` | the bound model refused, was switched or is unknown to its Provider |
| `writer_lost` | `cancelled` | the writer lost its lease before the Task finished |
| `deadline` | `expired` | the Task's wall clock ran out |
| `budget` | `exhausted` | a token, Attempt or verification-reserve limit ran out |
| `attempt_wall` | `exhausted` | a Worker reached its Attempt wall; a source-writing Worker's edits are kept |
| `collected` | the one it retained | the Task was collected; only its summary remains |
| `host_cancelled` | `cancelled` | the run was interrupted (Ctrl-C or a signal) |

Every state that ended names the one command to run next: the last line of `af task show` and
`af task run` (`next: af task run ID --retry-gate`), `none` when there is none, and
`state.next_action` under `--json`. It is `af task findings ID` after reviewer findings,
`af task run ID --retry-gate` after a failed gate that the kernel can run again on the same
candidate ([Retry a failed gate](#retry-a-failed-gate)), whatever its cause, `af task show ID
--checks` after one it cannot, `af task run ID` to resume a cancelled run (`af task start --file
TASK_FILE` once its interrupted node used up its recovery allowance), `af task plan --file
TASK_FILE --fit` after a budget or deadline ran out (with a new `task_id` once the Task ran),
`af provider status` or `af provider setup ID --login` after a Provider failure (`af provider
status` after `provider_environment`, an environment failure such as identity probes that proved
nothing after their retries, never an account change), and `af task run NEW_TASK_ID --from ID`
after `attempt_wall` when the Worker its wall stopped left edits
([An Attempt wall keeps the edits](#an-attempt-wall-keeps-the-edits)), else `af task show ID`. A command given
`--repo`, `--state` or `--workspace` prints an `af task` next command with them as typed, so it
runs as printed against the same Store; under the default Store it carries none. `af task show`
prints, per
failed node, its cause, the first line of its error and the artifact IDs of its whole diagnostic
and stderr; a node that only lacks the receipt of a branch that failed comes after that branch.
A command that exits with an outcome's code while its result goes to stdout, such as `af task
explain` or `af task run` of a Task that already finished, ends with the same line on stderr as
the run log does, `Task ID: finished/gate_failed/check_failed; next: af task run ID --retry-gate`:
a non-zero exit always says why on stderr.

`af task show ID --checks` prints the gate's last check receipt: each check's status, duration,
warm or cold directories, its warm layer when the policy has `[warm]` (the key, how the check came
by it, the source groups it rebuilt, its build and test phases and an eviction with its cause,
[Warm checks](task-execution/warm-checks.md)), the step that stopped it, its failing and flaky
tests and the last 40 lines of its stderr; `--json` prints one
[`af/task-checks@3`](../schemas/task-checks-v3.json) document. The gate's failed and flaky tests
and the step that stopped a check come from its report when the check writes one, and from the
text scan of its output only for a check without one (absent or refused): the runner's `FAIL`,
`TIMEOUT` and `SIGABRT` lines, its `flaky:` lines and its step log. A check line that prints this
evidence ends with where it came from, `evidence: report` or `evidence: output`
([ADR-0258](adr/0258-task-gate-evidence-comes-from-the-check-report.md)). `af task list --json`
rows carry the same `gate` summary, and after a failed gate `af task run` names the failing
tests, at most 20, then `… and N more; af task show ID --checks`.

A check the kernel stopped at its wall (its per-check wall, its check Attempt's wall or a warm
phase deadline, whichever bound its process) is recorded as such in the
[`af/TaskCheckReceipt@2`](../schemas/task-check-receipt-v2.json) under `stopped`: the cause
`wall`, the wall it ran under and how long it ran
([ADR-0255](adr/0255-say-a-check-stopped-at-its-wall-and-carry-typed-acceptance-in-the-check-receipt.md)).
The Task ends `finished/gate_failed/check_failed`, never `preflight_failed`, and `af task show`
(`why:`), `af task show --checks` (with `stop` under `--json`), `af task report` (the row's
detail) and the run's stderr each say `check kernel stopped at its 90 min wall after 88 min` and
what to do: make the check faster, or raise its wall in `.af/code-policy.toml`
(`check_wall_ms`, `check_process_wall_ms` or `[warm] build_wall_ms`) and plan the Task again;
`af task run ID --retry-gate` runs it again under the same wall. The wall is what the check's
process ran under, so it is what was left of the check's wall after preparation. Only a check that
did not start still says `never started`.

A Task's wall clock starts at its first `af task run`, not when it is planned: the preview says
`deadline starts when you run`, the Store moves the recorded deadline by the time the Task waited
for that run, so the confirmed plan is exactly the plan that runs, and the run prints the
deadline in local time with the time left (`deadline 17:41:18 +03 (2 h 59 min from now)`);
`af task show --json` carries it as `clock`. `af task plan` of a Task file whose content and bindings equal the planned Task's prints
the existing plan; a changed file under a planned ID is refused, and `af task plan --replace`
plans an unstarted Task again in place. A plan the Task's limits are too small for is refused
with the reason on stderr and the smallest limits that fit, computed from the reserve each
verification slot protects, a raised `wall_ms` keeping 5 s beyond the mandatory Attempts and the
reserve for the start of the run; `af task plan --fit` plans with them and prints what it raised,
and `--json` carries them as `planning_diagnostic.required_limits`.

Each Attempt runs under a token reservation that bounds it while it runs, and settles at the
usage its Provider reported. An Attempt whose Provider reported no usage — a Codex call that
failed with `Selected model is at capacity`, say, or one whose writer's lease expired while the
machine slept — is charged 0 and its usage is recorded as unknown, with its cause (`capacity`,
`rate_limit`, `authentication`, `model_unavailable`, `network`, `lease_expired`, `interrupted`
or `unreported`); it is never charged its reservation or an estimate
([ADR-0143](adr/0143-charge-zero-and-record-unknown-usage-when-no-usage-is-reported.md)). It
adds nothing to the Task's, a node's or the verification reserve's charged tokens, so only the
Attempt limits bound repeated failures of this kind. An Attempt that was abandoned, its writer
lost or its run interrupted, is charged what was reported too, never its reservation, and is
bounded by its node's recovery allowance instead ([below](#resume-an-interrupted-task)). `task list` prints such a Task's tokens as
`1200 (+1 unknown)` rather than as spend, `task show` adds the line
`tokens 1200 (+1 unknown: capacity)` naming the causes, and their `--json` documents carry
`unknown_usage_attempts`. A Claude Attempt whose bound model's safety classifier refused it
(`model_refused`) has known usage: it is charged what its result reports and is not retried,
because the same input is flagged again. A reply from a model the Worker did not bind is refused
and charged the same way, and a model ID Claude Code does not know fails as `model_unavailable`.
For each of these, `task run` and `review run` print one stderr line with the node, the Provider,
the bound model and effort, the classification and the next step; `task show` keeps the full
diagnostic ([ADR-0147](adr/0147-stop-the-claude-model-fallback-and-name-the-model-that-answered.md),
[model bindings](task-execution/model-bindings.md)). The
Task ID is also the delivery confirmation. An unverified Task is evidence, not a deliverable:
read `task show`, fix the catalog, check, Worker or goal problem, then start a corrected Task
under a new Task ID, or continue it from its reviewed candidate with `af task run NEW --from ID`
([Continue a reviewed Task](#continue-a-reviewed-task)).

### Which Tasks need you

`af task list --attention` answers "what needs me now?" for one Store. It groups every Task by
what it waits for, read from its state alone (phase, outcome, cause and whether it names a next
command) each time the Store is read; nothing is recorded. Each Task is in exactly one group:

| Group | Title | Needs you | Holds |
| --- | --- | --- | --- |
| `approval` | Waiting for approval | yes | planned, or waiting for a plan review, resources or input |
| `gate_failed` | Failed gate | yes | gate_failed: a required check failed or could not run |
| `findings` | Findings to answer | yes | changes_requested: reviewer findings or the evaluator |
| `deliver` | Verified, ready to deliver | yes | verified, with `af task deliver` as its next command |
| `stopped` | Stopped | yes | abandoned, provider_failed, cancelled, exhausted, expired |
| `running` | Running | no | a writer holds its lease; no next command until it ends |
| `nothing` | Nothing to do | no | delivered, verified with nothing to deliver, or collected |

The text prints each group that needs you and holds a Task under its title and count, one line
per Task with its ID, state and age, and its next command on the line below, broken between its
arguments to fit the terminal. Running Tasks are named on one line (`Running (1): docs-index`)
and the Tasks that need nothing are only counted (``Nothing to do (4): `af task list` shows
every Task``); when no Task needs you the first line says `No Task needs you now.`

```text
Failed gate (1)
  pagination-cli  finished/gate_failed/check_failed  3m
    next: af task run pagination-cli --retry-gate
Running (1): docs-index
Nothing to do (2): `af task list` shows every Task
```

`--json` prints one [`af/task-attention@1`](../schemas/task-attention-v1.json) document with all
seven groups in this order, the `running` and `nothing` groups included, each with its `group`,
`needs_you` and `tasks`, every Task the same `af/task-list-entry@4` that `af task list --json`
prints. `--attention` lists no sizes: with `--sizes` it is a usage error (exit 2). In the
browser (`af` alone at a terminal) the Tasks folder opens on the same groups under `NEEDS YOU`
([TUI design](design/tui.md#55-tasks)).

## Resume an interrupted Task

A run that stopped before its Task finished resumes with the same command, `af task run
TASK_ID`: after Ctrl-C or SIGTERM, after a closed lid or a reboot, after `af` was killed, or after
it lost its writer lease (the lease the one `af` process recording on a Task holds). The
interrupted Attempt is settled as *abandoned*: it is charged only the usage its Provider reported
before it stopped, never its reservation, which goes back to the Task's remaining tokens, and it
uses none of its node's Attempts. The node runs again, once by default; a slot that declares
`recovery_attempts` in an `af.pipeline/3` pipeline allows more. The run's first line says what
it does:

```text
resuming Task pagination-cli: node root.nodes.implement was interrupted at 17:41:03Z (its writer was lost); settling the lost Attempt and dispatching a new one (1 recovery of 1)
```

or why it cannot continue, with exit code 1:

```text
af task: cannot continue Task pagination-cli: node root.nodes.implement lost its writer 2 times (recovery allowance 1); fix: start it again with `af task start --file TASK_FILE` (the Task file it was started from), or allow more with `[slots.implementer] recovery_attempts` in its pipeline (af.pipeline/3)
```

A killed run holds its 60 s lease until it expires; `af task run` waits that out and says so.
Until a run resumes it, the Task's state is `abandoned/cancelled/writer_lost` (or
`host_cancelled`) in `af task show` and `af task list`, `af task show` adds `why: node N stopped
at T`, and the next command is `af task run TASK_ID`, or `af task start --file TASK_FILE` once a
node used up its recovery allowance; under `--json` that is the `state` object. On macOS `af task run` keeps the Mac
from idle-sleeping while it runs (`caffeinate`), and `af task run --verbose` says so. The whole
contract is in [interrupted runs](task-execution/interrupted-runs.md)
([ADR-0209](adr/0209-recover-a-lost-or-interrupted-attempt-without-charging-its-reservation.md)).

## Retry a failed gate

A Task whose last round ended at a failed gate, its check not passing and no reviewer or
evaluator run after it, ends its `task run` and `task show` output with the command that runs
that gate again
([ADR-0205](adr/0205-rerun-a-failed-test-alone-report-the-gate-and-retry-a-failed-gate-on-the-same-candidate.md)):

```text
check kernel: stopped at tests step test (exit 100); rerun it with: python3 scripts/nextest-gate.py --locked --profile ci --test-threads 7
check kernel failed twice: af::it task_heavy::heavy_review_cannot_erase_stale_claims
check kernel host: load 61.20 at start, 48.03 at end, 14 CPUs, 7 test threads
next: af task run pagination-cli --retry-gate
```

When the failure was the host's rather than the candidate's (a loaded machine, a stale pin you
have since fixed outside the Snapshot), `af task run TASK_ID --retry-gate` runs the failed check
again on the same sealed candidate, in a new check Attempt of the same round, under the plan you
confirmed: nothing is re-planned or re-admitted, no Provider is probed and no new Task is made.
When the check passes, the Task continues into its review or evaluation as if it had passed the
first time; when it fails again, it ends at the gate again. `task show` adds `gate retried: NODE
ran again on the same candidate`, and the Task's accounting keeps the failed check Attempt beside
the new one. `af task show ID --json` carries the same facts under `gate`: `check_reports`, each
check node's latest `af/TaskCheckReport@1`, and `retries`, every `TaskGateRetried@1`; `af task
report` counts each `--retry-gate` as a run of the Task, and an `af task refresh` as none. The retry gives the check node one Attempt more than its plan did, so the Task's own
Attempt, token and wall limits, and its Pipeline's, must still hold it. It is refused, with the
reason and nothing recorded, for a Task that was verified, delivered, has not finished, or whose
last failure was not the gate's (its checks passed, or a reviewer or evaluator ran after them),
and when the plan's approval or deadline has passed or the limits leave no room.

## Look at a candidate

`af task findings ID` prints the Task's last review round: per reviewer, each finding with its
severity, confidence, title, location, the quoted evidence and the proposed fix, grouped by
severity, then the evaluator's verdict on that round (the one the Task log recorded after it)
and its reason; `--round N` reads an earlier round with its own verdict, and `--json` prints
`af/task-findings@2`, whose findings each carry an `agent_prompt` and whose round says what it
covered: a reviewer that did not run shows as `not reviewed`, never as `no findings`.
`--format sarif` prints the same findings as SARIF 2.1.0
([ADR-0248](adr/0248-hand-review-findings-to-the-agent-that-will-fix-them.md)). `af task diff ID` prints the candidate's change against the
Task's source as the unified diff `git diff` prints for the same trees, without touching any
worktree; `--stat` and `--name-only` print its summary, `--snapshot` and `--base` pick two
Snapshots the Task recorded, and `--json` prints `af/task-diff@1`.

`af task materialize ID --into DIR` makes the tracked tree of the Git worktree `DIR` equal to the
candidate, with its writes, deletions and executable bits, and prints what it changed and
`next: git -C DIR status; af task report ID` (`af/task-materialized@1` under `--json`). It refuses
a `DIR` with uncommitted changes unless `--force`, and a candidate the Task did not verify unless
`--allow-unverified`, which then says `unverified candidate: review it before merging`. It never
commits or stages; `git -C DIR status` shows the change, ready for a person to commit.

```sh
af task findings pagination-cli
af task diff pagination-cli --stat
git worktree add ../pagination-review HEAD
af task materialize pagination-cli --into ../pagination-review
```

## Continue a reviewed Task

A Task that ended `changes_requested` holds a reviewed candidate and the findings against it.
`af task run NEW --from TASK` plans a new Task from TASK's own captured Task file
([ADR-0212](adr/0212-continue-a-task-from-its-reviewed-candidate-and-give-judging-workers-the-base.md)):

- its source is the candidate TASK's last review round reviewed (`--round N` names another), bound
  as a Snapshot exactly as an `inputs.source` binding of a derived tree is, not the base; its
  origin names the output that holds that candidate: TASK's result `snapshot`, or, for an earlier
  round whose candidate the result does not hold, the `af/TaskOutput@1` record that published it.
  A round whose candidate no recorded output holds is refused, never recorded under another
  Snapshot's output;
- its implementer receives that round's reviewer reports and the evaluator's verdict on that
  round (the one the Task log recorded after it, never another round's on the same Snapshot) as one
  `af/TaskReviewFindings@1` artifact on the Pipeline's optional `findings` input, each finding under the
  ID its reply answers by (`bugs-1`, `evaluator`), outside the 64 KiB requirements bound;
- its goal is TASK's goal, unless `--goal FILE` replaces it; nothing is pasted into it.

```sh
af task run pagination-2 --from pagination-cli            # plans; prints the plan
af task run pagination-2 --confirm-plan PLAN_ID           # runs it
af task run pagination-2 --from pagination-cli --round 1 --goal goal.md --execute
```

A Task file says the same with `"continue": {"task": "pagination-cli", "round": 1}`; it excludes
`inputs.source` and `inputs.findings`. TASK must be finished and must have recorded a review
round, or have kept the edits of a Worker its Attempt wall stopped (below), and for a reviewed
round the selected Pipeline must declare `findings` (the kernel's
`kernel/implementation-reviewed`, `kernel/implementation-ux` and `kernel/review-light` do, and so
does [`default/implementation`](#the-default-pipeline)), or the
plan is refused with the reason. `af task show NEW` prints `continues TASK round N`, `--json`
carries `continues` (`af/task-inspection@14`), and `af task report` names it in NEW's row.

### An Attempt wall keeps the edits

When a source-writing Worker, an implementer, reaches its Attempt wall before it answers, the
kernel seals the tree it left exactly as it seals an implementer's candidate (the same scratch and
ignore exclusions) and keeps it as a partial candidate, `af/TaskPartialCandidate@1`, named by the
Attempt's `af/TaskAttemptWallEnd@1` with the node's Attempt wall and the time the Attempt took
([ADR-0257](adr/0257-seal-the-implementers-tree-when-its-attempt-wall-ends-it.md)). Both are the
failed Attempt's raw evidence, never an output: a partial candidate is never sealed into a
result, checked, reviewed, accepted or delivered on its own. The Attempt is a failed one,
charged what its Provider reported (or zero with its usage unknown) and counted against its
node's Attempt limit, as before; it is no interruption, so no recovery allowance is spent.

The Task ends `exhausted/attempt_wall`, never `malformed_reply` or a Provider failure. `af task
show` prints `NODE: attempt_wall: implementer reached its Attempt wall of 30 min after 30 min;
its edits (3 paths) are kept as partial candidate sha256:…`, `--json` lists the record under
`attempt_wall_ends`, `af task run` prints `implement failed: its Attempt wall of 30 min ran out
after 30 min; its edits are kept as a partial candidate`, and `af task report` names the wall in
the Task's row. The next command continues from those edits:

```sh
af task run pagination-3 --from pagination-cli --execute   # starts from the partial tree
```

`--from` binds the partial tree as the new Task's source when TASK recorded no review round; the
new Task runs its whole pipeline on it, and its own result is what can be delivered. `af task
plan` prints each Worker slot's Attempt wall beside its model (`implementer: claude-opus-5/high
via claude, Attempt wall 30 min`), and `--json` lists them as `slots`, so a wall too short for
the goal is visible before the Task runs.

## The default pipeline

Every change to Afactory runs through the default Task pipeline that this repository's own
catalog, `.af/task-catalog.toml`, pins
([ADR-0253](adr/0253-the-default-task-pipeline-lives-in-the-catalog.md)), so any checkout plans
it. `default/implementation` is `kernel/implementation-ux` with its five slots bound to the
`default/*` Worker packages: one implementing iteration, the project's local gate on the sealed
candidate, two required reviewers, one advisory reviewer and an independent evaluator. It
declares `findings`, so `af task run NEW --from TASK` continues it. `default/verification` is
`kernel/verification-ux` with the same four judging slots and no implementer: it judges an
existing candidate read-only.

| Slot | Package | Model | Effort | What it does |
|---|---|---|---|---|
| `implementer` | `default/implementer` | `claude-opus-5-5` | `high` | one implementing iteration (`default/implementation` only) |
| `bugs` | `default/bugs` | `gpt-5.6-terra` | `high` | required reviewer: concrete functional defects |
| `correctness` | `default/correctness` | `gpt-6-sol` | `high` | required reviewer: requirements, contracts and architecture |
| `uix` | `default/advisor` | `gpt-6-astra` | `medium` | advisory reviewer: gate time and quality |
| `evaluator` | `default/evaluator` | `gpt-6-astra` | `high` | independent evaluator of the requirements |

The catalog's `[review.reviewers]` marks the `uix` slot advisory: its reviewer judges the time a
change adds to every future gate and pull-request validation, and the quality a maintainer cares
about, and files each finding at its honest severity. In this release its reports still count
toward convergence like any reviewer's: an open `blocker` or `major` of the bound advisory
reviewer keeps the round from converging, as a required reviewer's does; a kernel change that
stops them from counting is planned. The reviewers and the evaluator never receive the
implementer's summary: the tree, `../base`, the requirements and the check receipts are their
evidence.

To run it, name it in the Task file; `default/verification` takes a Task of `"kind":
"verification"` whose `inputs.source` binds the candidate it judges
([agents](agents.md#6-run-another-round)). The repository's `.af/af.toml` declares no
`[task.defaults]`, so the file carries its limits; these fit the pipeline's declared Attempt
allowances, and `af task plan --fit` prints the smallest that do:

```json
{
  "task_id": "issue-123",
  "kind": "implement",
  "goal": "Fix issue 123 as its text describes.",
  "pipeline": { "name": "default/implementation", "fallback": "refuse" },
  "limits": {
    "tokens": 8000000,
    "max_attempts": 16,
    "wall_ms": 43200000,
    "verification": { "tokens": 2000000, "attempts": 8, "wall_ms": 18000000 }
  }
}
```

The catalog binds `default/implementer` to the Provider label `claude-main` and the other four
packages to `codex-main`. When your machine-local registry does not carry those labels, map them
in a [local bindings](task-execution/local-bindings.md) file and pass it with `--bindings FILE`:

```toml
schema = "af.task-bindings/1"

[providers]
"default/implementer" = "claude-personal"
"default/bugs" = "codex-personal"
"default/correctness" = "codex-personal"
"default/advisor" = "codex-personal"
"default/evaluator" = "codex-personal"
```

## The tree before the change

Every Worker that judges a candidate without writing it, a reviewer, an evaluator or a verifier,
gets the tree before the change beside its own, read-only, at `../base`. A judging Worker whose
contract declares the optional `diff_subject` input also gets the Diff Subject there: one
`af/DiffSubject@1` artifact the kernel builds, whose `af/diff-subject@1` document lists each changed
path with its content digest on each side. No Pipeline binds that port. Before the Worker starts,
the kernel checks that the port holds exactly the change between the base and the Worker's source,
and refuses the Attempt when it is absent or differs
([ADR-0254](adr/0254-name-the-tree-of-a-workers-rules-and-deliver-the-diff-subject-as-a-typed-input.md)).
The kernel's `bugs`, `correctness`, `evaluator`, `ux` and `uix` Workers declare it;
`../diff-subject.json` is no longer written. The base is the source Snapshot's recorded base: a
sealed candidate's parent, or, for a bound tree, the parent of the tree it was bound from. A Task
file may name it instead with `"base": {"commit": "SHA"}`, captured at plan time as
`af/TaskBase@1`. The base is not under the Worker's tree, so it is never sealed back or counted
as an edit. A root capture with no declared base has no base, so its Workers get neither the base
nor a Diff Subject.

### What an Attempt survives

One call of an Attempt can fail and the Attempt still succeed
([ADR-0208](adr/0208-recover-an-attempt-from-a-transient-provider-failure-and-a-reply-that-does-not-parse.md)).
Everything below happens inside the Attempt's own wall and token reservation; the calls' usage
adds up, and nothing is reset or refunded.

- **A reply that does not parse.** A typed reply with prose around a valid JSON object is
  admitted as that object. Any other reply that does not parse is re-asked once from the same
  Worker, with the parser's message, line, column and the offending excerpt. Both replies are
  kept, and a second failure names them in the diagnostic: `raw reply kept: sha256:…`.
- **A structurally wrong reply.** A typed reply, and a review-Campaign reviewer's Reviewer
  Result, travels under a native schema: `--json-schema` for Claude Code, `--output-schema` for
  Codex ([ADR-0246](adr/0246-carry-the-reviewer-result-schema-natively-on-the-campaign-path.md)).
  The CLI refuses a reply of the wrong shape inside the same call, charged to it; the kernel
  still judges every reply it receives. A call that ends without a constrained reply fails the
  Attempt as `provider_failure`, charged the usage it reported, not as `invalid_output_contract`.
- **A transient Provider failure** — `network`, `capacity`, `timeout`, or an exit 1 that wrote
  nothing (`exited_without_output`) — is waited out and the call is made again: the wait the
  Provider asked for, else 60 s, then 120 s, then 240 s; at most three retries and ten minutes
  of waiting. While a wait lasts, `af task show` prints `NODE: waiting 120 s for capacity (retry
  2 of 3)`, and Ctrl-C ends it. A failure that outlasts the retries fails the node with its
  class and the retry history.
- **An identity probe that proves nothing.** Before every private send af checks that the
  Provider's account is still the one the plan captured. A probe that times out or answers
  malformed is sent again, three probes in all; then the node fails with `(environment)` in its
  line, as `provider_environment`, the environment's failure, not as an account change
  (`identity_changed`) or an unverified login (`identity_unverified`), and its next command is
  `af provider status`. A Provider CLI that updated itself, with the same account, passes; only
  another account, authentication method or organization fails the check, as
  `identity_changed`, with `run af task plan again`.
- **A refused admission probe.** When the bound model's safety classifier refuses the short
  admission probe itself, af sends it once more in other words; the refusal category, when the
  CLI names one, is in the diagnostic. Every Provider admission is decided before any other node
  starts, so no gate or implementer runs for a reviewer or evaluator that will be refused.
- **A report draft that fails its shape checks** — not exactly one draft, a citation set that
  is not sorted and unique, a required section missing by its exact heading — is refused before
  it is sealed, and the author's next Attempt receives the validation as feedback. The author's
  instructions state the report policy verbatim; its size and citations are judged by the
  report check after sealing, as before.

The shipped reviewer and evaluator slots (`kernel/implementation-reviewed`,
`kernel/verification-reviewed`, `kernel/verification`, `kernel/report`, `kernel/gate-bench`,
`kernel/experiment` and `kernel/experiment-trial`) allow two Attempts each, so a Worker whose
reply fails twice in one Attempt still has a second Attempt with typed feedback (issue #283).
Every Provider event is in `af task show --json` as `provider_events`; for each failed node,
`af task show` prints `NODE: CAUSE: FIRST LINE` (a retry history that ran out reads
`provider_capacity`, `provider_network` or `provider_timeout`), and a node that failed only
because the one it follows produced no receipt follows from the failure above and names that
node.

## Deliver the verified Snapshot

Choose an absent branch and an absent sibling path. The source checkout must still be clean at
the exact Task source commit.

```sh
task_id=pagination-cli
task_worktree="../$task_id"
task_receipt="${task_worktree}.delivery.json"
af task deliver "$task_id" \
  --repo . \
  --branch "af/$task_id" \
  --worktree "$task_worktree" \
  --confirm "$task_id" \
  --json > "$task_receipt"
```

Inspect `ignored_paths` in the JSON receipt before testing creates more files. These are
losslessly percent-encoded verified Snapshot paths that ordinary `git add` omits under the
repository and global ignore rules. They are identifiers, not literal pathspecs: decode their
filesystem bytes, then prefix Git's literal pathspec magic so names containing glob or
pathspec-magic characters identify only themselves.

```sh
python3 - "$task_receipt" "$task_worktree" <<'PY'
import json, os, subprocess, sys, urllib.parse

with open(sys.argv[1], encoding="utf-8") as receipt_file:
    encoded = json.load(receipt_file)["ignored_paths"]
paths = [urllib.parse.unquote_to_bytes(path) for path in encoded]
if paths:
    pathspecs = [b":(literal)" + path for path in paths]
    subprocess.run([b"git", b"-C", os.fsencode(sys.argv[2]), b"add", b"-f", b"--", *pathspecs], check=True)
PY
```

Alternatively, accept that the resulting commit differs from the verified Snapshot. Then inspect
and test the delivered worktree. It is deliberately uncommitted: committing, pushing and opening
a pull request remain human actions using the repository's normal Git controls.

A candidate verified in a second Task is delivered through that Task. A verified Task of kind
`verification` whose source it bound from another Task's candidate,
`"inputs": {"source": {"task": "pagination-cli", "port": "snapshot"}}`, and left unchanged,
delivers that candidate on the other Task's committed source: `af task deliver VERIFY_ID …` checks
the other Task as its own delivery would, and every delivery record names both Tasks through an
`af/TaskDeliveryProof@1`. Every other delivery still requires the Task's own Snapshot to descend
exactly from its source; a refusal names `af task materialize`, which writes a candidate into a
worktree of your choosing instead.

## Repeating and recovering a delivery

An exact repeat of the same command is safe. Before sealing, it verifies exact bytes or recovers
only an unchanged owned branch/worktree. After sealing, it verifies the durable delivery
identity and returns the same receipt even if the operator has since built, edited or committed;
that receipt attests to the original delivery, not the worktree's current bytes. A different
target for an already delivered Task conflicts.

If the process stopped after preparation, rerun the exact command. If recovery cannot prove that
partial content is delivery-owned, it records a terminal failure and preserves the original
branch and worktree. Retry the same explicitly confirmed Task with a new absent branch and
worktree; the next attempt releases only Afactory's internal ownership ref and never removes the
preserved target. Operator changes to an unsealed worktree are therefore never deleted.

## Warm checks

A code policy may declare a `[warm]` table
([ADR-0131](adr/0131-warm-task-checks-through-a-toolchain-keyed-bounded-cache.md)):

```toml
[warm]
build_cache = ["cargo_target", "cargo_home"]
caches = ["cargo"]      # optional: Cache Snapshots from machine policy
max_bytes = 8589934592  # optional: the eviction bound, 8 GiB by default; at most 32 GiB
hard_max_bytes = 17179869184  # optional: ends a running check; twice max_bytes by default
```

It grants a check directories that survive it. `cargo_target` becomes the check's
`CARGO_TARGET_DIR` and `cargo_home` its `CARGO_HOME`, Cargo's registry and git caches. Each is
`$XDG_CACHE_HOME/af/task-build-cache/<project>/<toolchain>/<kind>` (below `[storage]
build_cache_root` instead when the machine sets it), created with mode `0700` and
keyed by the repository and by the toolchain the check resolves. That toolchain is the
Snapshot's `rust-toolchain.toml`, `rustc -vV`, `cargo -vV`, the host triple, the check's
`PATH`, `LC_ALL`, `TZ` and `RUSTUP_HOME`, and the verified digest of a native toolchain the
check prepared ([ADR-0127](adr/0127-snapshot-pinned-rust-before-native-task-checks.md)). The
check's own runtime directory never reaches the key: wherever a hashed value names it, a fixed
token stands in its place, so two checks with the same toolchain share one key across Attempts,
Tasks and processes ([ADR-0144](adr/0144-hold-afs-disk-use-to-a-machine-budget.md)). A later
check with the same key starts from the earlier build. Both bounds cover the kinds of one key
together, not each on its own; the Storage Budget below bounds how many keys stay.

Under `[warm]` a check and its toolchain probe also receive the kernel's rustup home: its own
`RUSTUP_HOME`, else `$HOME/.rustup` when that directory exists. They also receive
`RUSTUP_AUTO_INSTALL=0`. The installed toolchain answers at once, and a toolchain the machine
lacks is `cold toolchain_unresolved`, never a download into the check's throwaway `HOME`. The
kernel only passes that path on. It never writes, bounds or removes the rustup home.

That toolchain is resolved once more before the Task's plan is admitted, so before any
implementer Attempt
([ADR-0205](adr/0205-rerun-a-failed-test-alone-report-the-gate-and-retry-a-failed-gate-on-the-same-candidate.md)):
when the source Snapshot's `rust-toolchain.toml` pins a toolchain the kernel's rustup home does
not hold, `af task start --execute` and `af task run` refuse the Task with the code policy file,
the `[warm] build_cache` field, the pinned toolchain and the probe's answer, and nothing runs or
is charged. Install it (`rustup toolchain install 1.88.0`) and run the Task again with `af task
run TASK_ID --confirm-plan PLAN_ID`. A `[rust_toolchain]` request with a machine mapping is
checked against the mapping the same way. A Snapshot that pins nothing names nothing to refuse.

The machine mapping is configured, not only exported: `[task] rust_toolchain_policy =
"~/.config/af/rust-toolchain.toml"` in the user's `~/.config/af/config.toml`, or in a project's
`.af/af.toml`, names the mapping file
([rust-toolchain.md](task-execution/rust-toolchain.md)), and
`AF_TASK_RUST_TOOLCHAIN_POLICY_FILE` still overrides every layer for one invocation. `af config
show --origin` names where it came from.

A declared `caches` kind gives the check an offline `CARGO_HOME` from the machine's cache
policy, as a Gate's `[gate] caches` do. That directory is materialized beside the check, never
into the source. It takes precedence over `cargo_home`: the two are never bound at once, and
the `cargo_home` kind records `cargo_home:superseded`.

`[warm]` refuses a great deal. It never runs with `require_container = true`: such a policy is
refused before any Worker starts. No directory ever enters a Worker sandbox, a Snapshot or a
delivered worktree, and its bytes never change a check's result. Before reuse the whole
directory is inspected without following links, relative to each parent's descriptor. It is
removed and recreated empty, so the check runs cold, when it is suspect. Suspect means a link, a
special file, another user's entry, a widened mode, a `credentials.toml` in a `cargo_home`, or
any entry the inspection cannot read. A check runs cold and says why when its toolchain cannot
be resolved, when another check holds its directory for 60 seconds, or when its directories are
already above `max_bytes`. The directories are measured again when the check ends and before
its result counts
([ADR-0135](adr/0135-collect-finished-tasks-behind-a-tombstone-and-a-reachability-sweep.md)).
Above `max_bytes` only, they are evicted and the check's own result stands; the next check runs
cold. Only `hard_max_bytes` ends a running check: one that grew them past it fails with
`warm_cache_bound_exceeded`, however fast it was, and so does one that left anything the
measurement cannot read. The directories are then removed, never trimmed, and the observation
names the bound that acted. This is candidate-built state on your machine, not isolation.

`af task show` prints one line per warm check, named even when the check never started:
`check kernel: passed in 812345 ms, cargo_target warm 2147483648, cargo_home warm 409600`, or
`check kernel: not_run, never started, cargo_target cold deadline_exhausted`. Deleting
`$XDG_CACHE_HOME/af/task-build-cache` is always safe. A policy without `[warm]` records every
document exactly as before.

## What a check may write

Every check af runs — a Task code check, a measurement repetition, a review gate check and a
post-apply integration check — gets four directories of its own, created empty and private
before it starts and removed when it ends
([ADR-0144](adr/0144-hold-afs-disk-use-to-a-machine-budget.md)):

| Variable | Directory |
| --- | --- |
| `HOME` | `<runtime>/home` |
| `TMPDIR` | `<runtime>/tmp` |
| `AF_CHECK_SCRATCH` | `<runtime>/scratch` |
| `XDG_CACHE_HOME` | `<runtime>/cache` |

`<runtime>` is `af-check-<pid>-<random>` in af's temporary directory; a killed `af` leaves it to
the next run's crash sweep. A container check sees the same variables below `/af-check`, on mounts
that go with the container. Write scratch files, copies of the tree and tool caches to
`$AF_CHECK_SCRATCH` or `$TMPDIR`, never to `/tmp` or `~/.cache`: nothing removes those, and a
check that copies its tree there on every run fills the disk. Build output that should survive a
check belongs in a `[warm]` directory or a Gate's `build_caches`. Review gate checks also get the
kernel's `RUSTUP_HOME` and `RUSTUP_AUTO_INSTALL=0`, as Task checks under `[warm]` do, so their
private `HOME` never downloads a toolchain; Cargo's registry comes from a Cache Snapshot or the
check's own `CARGO_HOME`.

## What a check reports

A Task code check also gets `AF_CHECK_REPORT`, a file in its runtime directory where it may
account for itself
([ADR-0205](adr/0205-rerun-a-failed-test-alone-report-the-gate-and-retry-a-failed-gate-on-the-same-candidate.md)):
one JSON object per line, each `"schema": "af.check-report/1"`. A `"record": "step"` line names
one step of the gate, its `stage` (`preflight`, `lint`, `build`, `tests` or `release`), its
`exit_code`, its `command` and, when the gate names one, the `fix` that repairs what it refuses;
a `"record": "tests"` line names the test threads, the tests that failed, and those that failed
in the parallel run and passed alone (`flaky`, with `"parallel": "failed", "isolated":
"passed"`). This repository's `scripts/ci-step.py` and `scripts/nextest-gate.py` write them.
A check writes one `tests` record per test run, and af folds them into one
([ADR-0258](adr/0258-task-gate-evidence-comes-from-the-check-report.md)): this repository's gate
runs `scripts/nextest-gate.py` once per test class and writes one per class. The folded
`failed` is the ordered union of the runs' names, each once, the first `MAX_NAMED_FAILED_TESTS`
(100). The folded `failed_count` is the number of distinct failed tests: when every run names all
of its failures (each run's `failed_count` below `MAX_NAMED_FAILED_TESTS`), it is the size of
that union: the names are complete, so a repeated name, inside one run or across runs, is one
test; when any run is capped (its `failed_count` at or above `MAX_NAMED_FAILED_TESTS`, so it
counts failures it does not name), it is the checked sum of the runs' counts less one for each
name a later run repeats from an earlier run; a name repeated inside one run is deduplicated in
`failed` but stays counted, because the capped run's unnamed failures make the true distinct
count unknowable and the fold never undercounts. A sum that does not fit `failed_count` refuses
the whole report, naming the line of the record that overflowed. Both branches keep the record
contract (`failed.len() <= failed_count`, and below the bound `failed.len() == failed_count`):
in the first the count equals the union's size by construction, so below the bound `failed` is
all of the union; in the second the count stays at or above the largest record's count, which
is at or above the bound, because the cross-run repeats subtracted are names earlier records
added and counted, so only `failed.len() <= failed_count` applies and the union's size never
exceeds the summed counts less those repeats. The fold keeps the runs' flaky
names in order, each once, at most 25; takes the first run's `test_threads`; and takes the strongest
`rerun`, `over_bound` before `isolated` before `not_run` before `not_needed`. Every record must
hold on its own: one that breaks the contract refuses the whole report, wherever it stands.

After the check, af records an `af/TaskCheckReport@1`
([schema](../schemas/task-check-report-v1.json)) beside its `af/TaskCheckReceipt@2`: for each
local check that started, the 1-minute load average when it started and ended and the CPUs
available to af, which af measures itself, and what the check wrote, `admitted`, `absent` when it
wrote nothing, or `refused` with the reason when its lines are not this contract. A check's own
account never changes its result; its exit code does. The gate's failed and flaky tests and the
step that stopped a check come from its report when the check writes one, and from the text scan
of its output only for a check without one (absent or refused). The cause is `preflight_failed`
when every failed check stopped at a step before its tests and named no failed test: a reported
step by its stage (`preflight`, `lint`, `build`), a step read from a log by its label; a check the
kernel stopped at its wall is `check_failed` (or `check_flaky_then_failed`) first. The nested
output of a project's own preflight, such as the failing fixture gates
`scripts/test-nextest-gate.py` prints, never becomes a Task's failing or flaky test when the
check writes a report. `af task show` prints, per check, where it
stopped (`check kernel: stopped at preflight step performance-kernel-harness (exit 1); fix:
python3 -I scripts/performance/cli.py harness-digest --write`), its flaky tests and the tests
that failed twice, each on its own line, and the host's load beside a check that did not pass or
had flaky tests.

## The Storage Budget

af keeps at most `[storage] max_bytes` between runs
([ADR-0144](adr/0144-hold-afs-disk-use-to-a-machine-budget.md)). The default is `"auto"`: 20 GiB,
or 40 GiB when the volume holding the warm build keys has more than 100 GiB free. One warm key of
a Rust workspace like this one is 11-16 GiB, and a 20 GiB budget evicted it after every run, so
every gate built cold; 40 GiB holds one 16 GiB key with half its size to spare and 16 GiB of
records beside it. `af storage` says which it chose and why. The budget counts warm toolchain
keys, warm Workspaces, review campaign Stores, the finished Tasks of every Task Store, Stores
this release cannot read, and installed versions. When af holds more, caches go first (warm
keys, warm Workspaces, installed versions), then telemetry, then records (review campaigns,
unreadable Stores, Surveys, finished Tasks), least recently used first within each. Nothing in
use goes — a held lock, a check holding a warm key, a live writer lease (judged when the sweep
takes stock, before it plans), a running `af review run`, the default or a pinned version, the
running binary — and nothing used within the last hour. Nor does the newest warm key of a
project whose checks used it in the last 7 days, a finished Task younger than `[storage]
keep_days` (14), a finished Task a registered delivered Workspace was made from (the Task behind
a delivered branch and its pull request), or a Project Store another release wrote, with or
without Workspaces (ADR-0158, ADR-0159). When the largest warm key does not fit `max_bytes` with
half its size to spare, `af storage` and the run say so once: `warm key 16.1 GiB does not fit
[storage] max_bytes 20.0 GiB: gates run cold whenever the budget must evict it; fix: raise
max_bytes to ≥ 25 GiB or lower [warm] max_bytes`. A finished Task goes through the Store's own
collection and keeps its tombstone. Every
directory is removed through descriptors opened from its configured root (`/` for a Store
registered elsewhere) without following a link, and only while it is still the directory the
sweep measured; anything else is left and reported. An installed version is checked again under
the lock `af self` holds while it changes the default or a pin, so one that became protected
stays.

`af storage` prints what af holds, per kind, against the budget, and the free bytes against
`[storage] min_free_bytes` (10 GiB by default); `--json` prints one `af/storage@5` document,
whose `volumes` name each root the floor measures (`sandbox`, `cache`, `build_cache`) with its
volume, free bytes, `below_floor` and `next_action`, and whose `unreadable_store` entries carry
the `writer_release` their markers name and the `next_action` that reads them. While a Project
Store cannot be read, a sweep collects no finished Task, since any could be the source of one of
its delivered Workspaces, and it names that Project Store; a campaign whose writer leases cannot
be read is in use.
After every `af task run` and `af review run` the sweep holds af to `max_bytes`, evicting in the
order above; it does not collect by age unless `[storage] auto_gc = true`. A Task command prints
one line for all its sweeps, and only when they did something: `storage: freed 1.2 GiB (2 warm
keys)`, with what failed or what is still over the budget after it; `af task run --verbose` and
`af task start --execute --verbose` print every removal instead, as `af storage prune` does, and
only they repeat the Task's recorded `storage sweep:` line in a run's output. `af storage
prune` previews the sweep with collection; `af storage prune --apply` performs it now. It collects
first: finished Tasks beyond the
newest `keep_tasks` (20) of each Store, review campaigns beyond the newest `keep_campaigns` (20),
the Surveys of each Project beyond the newest `keep_surveys` (50) and Stores this release cannot
read, each idle at least `keep_days` (14); then it evicts until af fits. A Survey goes behind its
`SurveyCollected@1` tombstone, and never the newest complete one ([Collection](project.md#collection)). A Store made with `--state` is recorded in `$XDG_STATE_HOME/af/stores.toml`, so the
sweep reaches it too; it counts only af's own entries, and it is removed whole only while it
carries the owner marker `.af-store` af wrote when it created the directory and holds nothing
else. Otherwise only af's entries go, and the sweep names what it left in place. A finished
Task whose remote checks left gate branches or a pull request is
collected only after its gate cleanup is done, by the sweep and by `af task gc --apply` alike;
until then it stays and the next sweep tries again. A Task cancelled or abandoned after its gate
published is cleaned up the same way once no writer holds its lease. The sweep that ends `af task
run` records what it removed and what failed on that Task as one `storage_sweep` observation:
`af task show` prints a `storage sweep:` line and `--json` carries `storage_sweeps` (a telemetry
segment it removed is counted there among the omitted removals, never listed under another kind).
Telemetry is the kind `telemetry`: every registered telemetry directory, evicted segment by
segment under the same rules and past its own `[telemetry] max_bytes`, with every applied sweep
reclaiming its temporary files, unreferenced blobs and free index pages; `--json` of
`af storage prune` (`af/storage-prune@4`) carries that part as `telemetry`
([docs/telemetry.md](telemetry.md)).

Below the free-disk floor af does not start a check, a measurement repetition or a Worker
Attempt. Every volume af works on counts — the one holding its sandbox root (`[storage]
sandbox_root`, else the temporary directory, `TMPDIR`), the one holding `$XDG_CACHE_HOME` and the
one holding `[storage] build_cache_root` when it is set — and a volume whose free bytes cannot be
measured refuses too, with the measurement error. A refusal names the volume by its mount point
and file system type, both numbers and what af keeps there: `insufficient_disk: /tmp (tmpfs): 1.9
GiB (2040109465 bytes) free, floor 10.0 GiB (10737418240 bytes); the sandbox root is /tmp; set
TMPDIR or [storage] sandbox_root to a volume with room`. A configured `sandbox_root` is where
every sandbox and check runtime directory of a Task or review run is made, created private when
it is missing. The check's result is `not_run` with reason `insufficient_disk: …`; a
measurement fails with reason `insufficient_disk` without starting its command; a Worker Attempt
is released before it starts and charged nothing, and `af task run` stops with the same message,
leaving the Task to resume once there is room. `[storage]` is read only from the machine's layers: `/etc/af/config.toml`,
`~/.config/af/config.toml` and `AF_STORAGE__<KEY>`, for example
`AF_STORAGE__MAX_BYTES=50GiB`. A repository's `.af/af.toml` cannot change it; `af config show`
reports such a table as ignored. See `af help storage` for every key.

## Reclaim Store space

`af task list --sizes` prints, per Task, the bytes of the stored objects only that Task reaches
and the bytes it shares with other Tasks or Campaign records, then the Store's total. `af task
gc --older-than 14 --keep 5` previews which finished Tasks beyond the newest five, idle for 14
days, it would collect, how many bytes that frees, and why every other Task stays: running,
unfinished, holding a writer lease or bound by another Task's `inputs`. It writes nothing. With
`--apply` it writes one tombstone per collected Task and removes every object no remaining Task
or Campaign record reaches
([ADR-0135](adr/0135-collect-finished-tasks-behind-a-tombstone-and-a-reachability-sweep.md)). A
collected Task keeps its ID, kind, revision, outcome, spend, how many of its Attempts' usage was
unknown, and times: `task list` and `task show` print it as `collected <time>`, still with
`(+N unknown)` beside its tokens, and `task output` and `task deliver` refuse it. Run it
between Tasks: `--apply` is refused while any Task's writer lease is live. If it stops midway,
rerun it; the next run finishes the removal. The Storage Budget's sweep runs the same collection
by itself after every run.

## Report what Tasks cost

`af task report TASK_ID...` summarizes what one or more Tasks of one Store cost and how they ran,
as one Markdown block for a pull request description
([ADR-0142](adr/0142-carry-the-af-task-report-in-every-pull-request.md)). It takes the same
`--repo` and `--state` selectors as `task show`, reports the Tasks in the order given, and refuses
an unknown Task ID by name without printing anything else. It only reads the Store: no Worker
runs, no Provider is contacted, nothing is written.

```sh
af task report pagination-cli
af task report implement-x verify-x --json
```

The block leads with one line per pipeline the Tasks ran: `**name@version**:` and its steps in
dependency order, each named by its role with its Worker in parentheses (`codex
gpt-6-sol/high`, or `command`), the gate with its check names, and parallel steps of one role
grouped (`review (bugs, correctness: codex gpt-6-sol/high)`); Provider admission is left out.
A Task whose plan is no longer retained (it was collected) or that never reached planning names
no pipeline, and one line `**unknown pipeline**: not retained` stands for all such Tasks.
Then each Task is a round, numbered in the order given, with one row: its state as `task show`
states it, followed for a Task that did not verify by why (`finished/gate_failed/check_failed:
check kernel, 2 tests: a::b, c::d`), its review findings, chargeable tokens and active time. A Tokens cell, in a round, the
`Total:` row or a node, is the charged total followed by `(+N unknown)` when N of its Attempts
reported no usage, and the round's details name their causes
(`1 Attempt's usage unknown (capacity)`); a collected Task keeps the count, not the causes. **Findings** counts what the
Task's review rounds recorded, by severity (`6 major, 1 minor`), each finding once as the round's
reduce step wrote it; it reads `none` when the reviewers ran and their complete rounds found
nothing, `unknown` when the review ran but a round has no complete finding set (a required
reviewer's result is missing, say) or none was recorded yet, `gate failed` when a check failed and
the review did not run, and `—` for a Task without a review, followed by `; N reviewer(s)
failed` when a reviewer's Attempt failed. A round recorded before a `task
refresh` still counts. A last `Total:` row sums Attempts (with how many failed), tokens and
active time. **Active time** is the sum of the Task's runs, each from its first event to its
last: a run is `task start --execute` or one `task run` that started an Attempt or recorded a
gate retry (`task run --retry-gate`), so a Task that
waited a day before it was resumed shows that day in its wall time only, and neither a `task
refresh`, even when it settles an Attempt an interrupted run left pending, nor a resume that only
publishes an already selected result is a run. **Wall time** runs from the Task's first recorded
event to its last. Each round then has a collapsed `<details>` element with its runs, wall time
and failed Attempts by reason class (`provider_failure`, `process_failure`, …) with the tokens
charged to them, and its nodes: role, Worker, Attempts, tokens, elapsed time and the checks the
gate ran with their durations. Each Attempt counts toward the Worker of the plan it ran under, so
a node that a refresh bound to another Worker has one row per Worker. Token counts read with
thousands separators (`205,295`). A figure the Store does not record reads `unknown`; nothing is
estimated. For a Task resumed once after an interrupt it prints:

```markdown
<!-- af-task-report:v1 -->
### af task report

**fixture/implementation@1.0.0**: implement (command) → gate (pagination) → evaluate (command)

| Round | Task | Outcome | Findings | Tokens | Active |
| ---: | --- | --- | --- | ---: | ---: |
| 1 | pagination-cli | finished/verified | — | 0 | 1.8s |
|  | Total: 4 Attempts (1 failed) |  |  | 0 | 1.8s |

<details>
<summary>Round 1 · pagination-cli: 2 runs, 3.5s wall, 1 failed Attempt (1 process_failure; 0 tokens)</summary>

| Node | Role | Worker | Attempts | Tokens | Elapsed | Checks |
| --- | --- | --- | ---: | ---: | ---: | --- |
| root.nodes.implement | implement | command | 2 (1 failed) | 0 | 258ms | - |
| root.nodes.check | check | - | 1 | 0 | 62ms | pagination passed 52ms |
| root.nodes.evaluate | evaluate | command | 1 | 0 | 96ms | - |

</details>
<!-- /af-task-report -->
```

Paste the whole block, both markers included, as plain text: the `PR report` check does not
count a block inside a code fence or indented as code, including one indented four spaces right
after a heading, a thematic break, a fence, an HTML block or a list item. Providers appear only as kind, model and effort,
and a recorded model that is not a model identity (a path or a URL, say) reads `unknown`: the
report never carries a Provider label, a path, a credential, a prompt or Worker output.
`--json` prints the same figures as one
[`af/task-report@3`](../schemas/task-report-v3.json) document, with exact decimal tokens and
times in milliseconds, the `pipelines` with their steps, and each Task's `round`, `state`,
`detail` and `findings`;
an unknown figure is absent there.

`af task report TASK_ID... --pr-body` prints the whole pull request description instead, which
`gh pr create --body-file` takes unchanged: the repository's `.github/PULL_REQUEST_TEMPLATE.md`
(or `--template FILE`) with this block between its markers. The report block opens with a
receipt before its pipeline and rounds: outcome, passed,
failed and skipped checks, unresolved findings by severity, tokens with unknown usage marked,
and active time. The lines added block is what the checkout's `scripts/pr-lines.py` prints
between `<!-- af-pr-lines:v1 -->` and
`<!-- /af-pr-lines -->` when the checkout has the script, counted from `--base REF` (origin/main
by default). A checkout without the script keeps the template's block as it is; one whose script
cannot print its block gets no body, and the command exits 1 naming `--base REF`; a block
`scripts/check-pr-report.py` would refuse gets no body either, and the exit 1 names what is
missing. A whole block is the begin marker, then the title `**Lines added: <figure>**`, the
header `| Directory | Added | Share |`, its separator and at least one directory row, each
directory once, or `**Lines added: none**` alone, then the end marker.
`scripts/check-pr-report.py --body` accepts it as it is.

## Troubleshooting

| Symptom | Meaning and action |
|---|---|
| `only a verified Task can be delivered` | Inspect the Task's findings (`af task findings ID`), checks and evaluator, then start a corrected Task; `af task materialize ID --into DIR --allow-unverified` writes the candidate where you can look at it. |
| `HEAD no longer equals the Task source revision` | Return the source checkout to the recorded commit or start a Task from the new commit. |
| `worktree is not clean` or `staged changes` | Preserve or commit the operator's work elsewhere, then retry from an exact clean source. |
| branch/path already exists | Choose a new absent target; Afactory never overwrites either one. |
| incomplete rollback/recovery | Rerun the exact command once. If it reports a preserved terminal failure, retain the original branch/worktree and retry the confirmed Task with a new absent target. |
| Worker authentication failure | Re-establish the named machine-local Provider login yourself with `af provider setup … --login` at a private terminal, then confirm with `af provider status`. |
| missing check tool | Install the repository-approved toolchain; never remove or weaken the check. |
| state or disk error | Preserve the external Task state directory, restore disk capacity/permissions, and retry the exact inspection or delivery command. |
