# Implementation Tasks with `af task`

`af task` runs one bounded implementation Task against a local Git repository and, when the
result is verified, delivers it to a new local branch and worktree for a human to inspect and
commit. A Task starts from a Task file that names its ID, kind, goal and limits; the Pipeline and
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
- an installed `af` release, verified against the release's `SHA256SUMS` and
  `SHA256SUMS.minisig` (the release's `install.sh` does this);
- machine-local Provider authentication for every model Worker the Pipeline binds, checked with
  `af provider status` and established as [`providers.md`](providers.md) describes — an
  interactive login is opt-in and happens only at your own terminal; command Workers need none;
- every tool named by the code policy's checks, such as the repository's compiler and test
  runner;
- a reviewed Task catalog committed in the repository: `.af/task-catalog.toml`, the Pipeline and
  Worker packages it pins, and `.af/code-policy.toml` with the required checks.

`af catalog init --profile software --destination task-demo` creates a new starter directory,
which must not exist yet, holding a working, credential-free catalog and runnable Task files
([starters](task-execution/starters.md)); it does not add a catalog to the current repository.
`af catalog sync` imports shared definitions from Git
([shared catalogs](task-execution/shared-catalogs.md)). The catalog pins every package by digest,
so changing any package byte requires a newly reviewed pin. Checks are project-specific, so the
code policy is written per repository.

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

The Task ID is chosen here and must be new to the state directory. Without `pipeline`, planning
[selects](task-execution/selection.md) a fitting Pipeline from the catalog. The goal and any
`requirements` are input data; they cannot change the committed execution permissions. The
[Task-file walkthrough](task-execution/task-file.md) runs this example end to end.

## Start and inspect a Task

Run from the clean repository at committed `HEAD`. State defaults to a directory outside the
repository under the user's XDG state directory; `--state` selects another external directory.
Save the Task request outside the checkout too (for example, `../ticket.json`); Afactory captures
its contents into Task state. `--file` selects the request, not the source repository, so remain
in the source checkout or select it with `--repo`.

```sh
af provider status
af task start --file ../ticket.json
# Review the preview and copy its complete Plan identity.
af task run pagination-cli --confirm-plan PLAN_ID
af task list
af task show pagination-cli
```

The [ASCII preview guide](task-execution/preview.md) covers the two plan views and agent
approval. `task list` shows terminal outcome, chargeable tokens and current delivery state.
`task show` adds exact Snapshot IDs and the append-only event history. Both accept `--json`. The
Task ID is also the delivery confirmation. An unverified Task is evidence, not a deliverable:
read `task show`, fix the catalog, check, Worker or goal problem, then start a corrected Task
under a new Task ID.

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
`$XDG_CACHE_HOME/af/task-build-cache/<project>/<toolchain>/<kind>`, created with mode `0700` and
keyed by the repository and by the toolchain the check resolves. That toolchain is the
Snapshot's `rust-toolchain.toml`, `rustc -vV`, `cargo -vV`, the host triple, and the check's
`PATH`, `LC_ALL`, `TZ` and `RUSTUP_HOME`. A later check with the same key starts from the
earlier build. Both bounds cover the kinds of one key together, not each on its own.

Under `[warm]` a check and its toolchain probe also receive the kernel's rustup home: its own
`RUSTUP_HOME`, else `$HOME/.rustup` when that directory exists. They also receive
`RUSTUP_AUTO_INSTALL=0`. The installed toolchain answers at once, and a toolchain the machine
lacks is `cold toolchain_unresolved`, never a download into the check's throwaway `HOME`. The
kernel only passes that path on. It never writes, bounds or removes the rustup home.

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

## Reclaim Store space

`af task list --sizes` prints, per Task, the bytes of the stored objects only that Task reaches
and the bytes it shares with other Tasks or Campaign records, then the Store's total. `af task
gc --older-than 14 --keep 5` previews which finished Tasks beyond the newest five, idle for 14
days, it would collect, how many bytes that frees, and why every other Task stays: running,
unfinished, holding a writer lease or bound by another Task's `inputs`. It writes nothing. With
`--apply` it writes one tombstone per collected Task and removes every object no remaining Task
or Campaign record reaches
([ADR-0135](adr/0135-collect-finished-tasks-behind-a-tombstone-and-a-reachability-sweep.md)). A
collected Task keeps its ID, kind, revision, outcome, spend and times: `task list` and `task
show` print it as `collected <time>`, and `task output` and `task deliver` refuse it. Run it
between Tasks: `--apply` is refused while any Task's writer lease is live. If it stops midway,
rerun it; the next run finishes the removal.

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
Then each Task is a round, numbered in the order given, with one row: its outcome as `task show`
states it, its review findings, chargeable tokens and active time. **Findings** counts what the
Task's review rounds recorded, by severity (`6 major, 1 minor`), each finding once as the round's
reduce step wrote it; it reads `none` when the reviewers ran and their complete rounds found
nothing, `unknown` when the review ran but a round has no complete finding set (a required
reviewer's result is missing, say) or none was recorded yet, `gate failed` when a check failed and
the review did not run, and `—` for a Task without a review, followed by `; N reviewer(s)
failed` when a reviewer's Attempt failed. A round recorded before a `task
refresh` still counts. A last `Total:` row sums Attempts (with how many failed), tokens and
active time. **Active time** is the sum of the Task's runs, each from its first event to its
last: a run is `task start --execute` or one `task run` that started an Attempt, so a Task that
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
| 1 | pagination-cli | verified | — | 0 | 1.8s |
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
[`af/task-report@1`](../schemas/task-report-v1.json) document, with exact decimal tokens and
times in milliseconds, the `pipelines` with their steps, and each Task's `round` and `findings`;
an unknown figure is absent there.

## Troubleshooting

| Symptom | Meaning and action |
|---|---|
| `only a verified Task can be delivered` | Inspect the Task's checks and evaluator, then start a corrected Task. |
| `HEAD no longer equals the Task source revision` | Return the source checkout to the recorded commit or start a Task from the new commit. |
| `worktree is not clean` or `staged changes` | Preserve or commit the operator's work elsewhere, then retry from an exact clean source. |
| branch/path already exists | Choose a new absent target; Afactory never overwrites either one. |
| incomplete rollback/recovery | Rerun the exact command once. If it reports a preserved terminal failure, retain the original branch/worktree and retry the confirmed Task with a new absent target. |
| Worker authentication failure | Re-establish the named machine-local Provider login yourself with `af provider setup … --login` at a private terminal, then confirm with `af provider status`. |
| missing check tool | Install the repository-approved toolchain; never remove or weaken the check. |
| state or disk error | Preserve the external Task state directory, restore disk capacity/permissions, and retry the exact inspection or delivery command. |
