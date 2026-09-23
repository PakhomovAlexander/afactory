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

```sh
af provider status
af task start --file ticket.json
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
