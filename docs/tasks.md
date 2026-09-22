# Implementation Tasks with `af task`

`af task` runs one bounded implementation Task against a local Git repository and, when the
result is verified, delivers it to a new local branch and worktree for a human to inspect and
commit. This page covers the sequential `implement` Task started from a goal. Task files
(`--file`), embedded Review, generated plans and the other Task kinds are described under
[Task execution](task-execution.md).

## Supported boundary

One Task runs one local sequential implementation:

1. one locked implementer edits a private sandbox;
2. required read-only acceptance Gates inspect the derived Snapshot;
3. one separately locked evaluator approves or rejects it without the implementer transcript;
4. an approved result remains an immutable verified Snapshot until an operator explicitly
   delivers it;
5. delivery creates a new local branch and linked worktree containing those exact bytes as
   uncommitted work.

Delivery never changes the source checkout, commits, pushes, opens a pull request, invokes a
remote, or receives repository credentials
([ADR-0031](adr/0031-deliver-verified-tasks-to-new-local-worktrees.md)). Parallel Workers,
hosted execution and automatic Integration are outside this path.

## Prerequisites

- a local Git clone with no staged, tracked or untracked changes at the commit used to start
  the Task;
- an installed `af` release, verified against the release's `SHA256SUMS` and
  `SHA256SUMS.minisig` (the release's `install.sh` does this);
- machine-local Provider authentication for the Workers named by the implement pipeline,
  checked with `af provider status` and established as [`providers.md`](providers.md) describes
  — an interactive login is opt-in and happens only at your own terminal;
- every tool named by the repository's acceptance Gate, such as its compiler and test runner;
- a reviewed `.af/` directory committed in the repository: `af.toml`, `af.lock`, the implement
  pipeline and both Worker packages.

The `.af/af.lock` digests must match the committed pipeline and Worker bytes; changing any of
those bytes requires a newly generated and reviewed lock. Acceptance Gates are project-specific,
so this bundle is produced per repository. `af onboard` generates a repository's two-Worker
pull-request review authority ([ADR-0032](adr/0032-generate-review-authority-with-af-onboard.md));
the implementer/evaluator pipeline is maintainer-reviewed rather than generated.

Trusting that committed authority and running `af task start --execute` authorizes Afactory to deliver the
implementer and evaluator their exact declared inputs for every stage of the Task
([ADR-0033](adr/0033-configured-workers-authorize-declared-input-delivery.md)). There is no
additional per-Worker or per-call consent prompt. Delivery, Provider rebinding and any other
remote side effect remain separate explicit operations.

The [ASCII preview guide](task-execution/preview.md) covers the two views and agent approval.

## Start and inspect a Task

Run from the clean repository at committed `HEAD`. State defaults to a directory outside the
repository under the user's XDG state directory; `--state` selects another external directory.

```sh
af provider status
af task start --kind implement --goal "describe one bounded change and its acceptance" \
  --authority HEAD
# Review the preview and copy its Task and Plan identities.
af task run TASK_ID --confirm-plan PLAN_ID
af task list
af task show task-0123456789abcdef0123
```

`task list` shows terminal outcome, chargeable tokens and current delivery state. `task show`
adds exact Snapshot IDs and the append-only event history. Both accept `--json`. Keep the
returned Task ID: it is also the delivery confirmation. An unverified Task is evidence, not a
deliverable: read `task show`, fix the repository authority, Gate, Worker or goal problem, then
start a new Task.

## Deliver the verified Snapshot

Choose an absent branch and an absent sibling path. The source checkout must still be clean at
the exact Task source commit.

```sh
task_worktree=../task-0123456789abcdef0123
task_receipt="${task_worktree}.delivery.json"
af task deliver task-0123456789abcdef0123 \
  --repo . \
  --branch af/task-0123456789abcdef0123 \
  --worktree "$task_worktree" \
  --confirm task-0123456789abcdef0123 \
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
| `only a verified Task can be delivered` | Inspect the Task's Gates/evaluator and start a corrected Task. |
| `HEAD no longer equals the Task source revision` | Return the source checkout to the recorded commit or start a Task from the new commit. |
| `worktree is not clean` or `staged changes` | Preserve or commit the operator's work elsewhere, then retry from an exact clean source. |
| branch/path already exists | Choose a new absent target; Afactory never overwrites either one. |
| incomplete rollback/recovery | Rerun the exact command once. If it reports a preserved terminal failure, retain the original branch/worktree and retry the confirmed Task with a new absent target. |
| Worker authentication failure | Re-establish the named machine-local Provider login yourself with `af provider setup … --login` at a private terminal, then confirm with `af provider status`. |
| missing Gate tool | Install the repository-approved toolchain; never remove or weaken the Gate. |
| state or disk error | Preserve the external Task state directory, restore disk capacity/permissions, and retry the exact inspection or delivery command. |
