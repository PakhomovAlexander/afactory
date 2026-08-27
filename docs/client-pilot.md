# Trusted design-partner pilot

This runbook is the minimum supported operating path for the first Afactory client pilots. It is
not a general-availability contract: the pilot owner supplies and reviews each repository's
digest-pinned `.af/` authority, and the repository and Provider authentication remain under the
client operator's control.

## Supported boundary

The pilot supports one local sequential implementation Task:

1. one locked implementer edits a private sandbox;
2. required read-only acceptance Gates inspect the derived Snapshot;
3. one separately locked evaluator approves or rejects it without the implementer transcript;
4. an approved result remains an immutable verified Snapshot until an operator explicitly
   delivers it;
5. delivery creates a new local branch and linked worktree containing those exact bytes as
   uncommitted work.

Delivery never changes the source checkout, commits, pushes, opens a pull request, invokes a
remote, or receives repository credentials. Parallel Workers, hosted execution, automatic
Integration, and self-service configuration are outside this pilot.

## Prerequisites

- a trusted local Git clone with no staged, tracked, or untracked changes at the commit used to
  start the Task;
- a pilot release and SHA-256 file supplied through the private
  `PakhomovAlexander/afactory` release;
- existing `gh` access to that private release; no token is copied into the repository;
- machine-local Codex authentication for the default implementer and evaluator Workers;
- every tool named by the repository's acceptance Gate, such as its compiler and test runner;
- a maintainer-reviewed `.af/` directory committed in the repository, including `af.toml`,
  `af.lock`, the implement pipeline, and both Worker packages.

The `.af/af.lock` digests must match the committed pipeline and Worker bytes. Pilot onboarding
produces this bundle per repository because acceptance Gates are project-specific; changing any
of those bytes requires a newly generated and reviewed lock.

## Install or update

Use a fresh staging directory and substitute the exact pilot tag and release host triple supplied
by the pilot owner:

```sh
set -eu
pilot_version=vX.Y.Z
pilot_host=aarch64-apple-darwin
pilot_stage="$(mktemp -d)"
gh release download "$pilot_version" --repo PakhomovAlexander/afactory \
  --pattern "af-${pilot_version}-${pilot_host}.tar.gz*" --dir "$pilot_stage"
cd "$pilot_stage"
shasum -a 256 -c "af-${pilot_version}-${pilot_host}.tar.gz.sha256"
tar -xzf "af-${pilot_version}-${pilot_host}.tar.gz"
pilot_bin_dir="${XDG_BIN_HOME:-$HOME/.local/bin}"
mkdir -p "$pilot_bin_dir"
install -m 0755 af "$pilot_bin_dir/af-${pilot_version}"
ln -sfn "af-${pilot_version}" "$pilot_bin_dir/af"
"$pilot_bin_dir/af" --version
```

On Linux, use `sha256sum -c` instead of `shasum -a 256 -c`. Updating repeats the same procedure
with a new versioned binary; rollback repoints the `af` symlink to the preceding verified version.
Never install an archive whose checksum does not pass.

## Run and inspect a Task

Run from the clean repository at committed `HEAD`. State defaults outside the repository under
the user's XDG state directory; `--state` may select another external directory.

```sh
af provider status
af task start --kind implement --goal "describe one bounded change and its acceptance" \
  --authority HEAD --json
af task list
af task show task-0123456789abcdef0123
```

`task list` shows terminal outcome, chargeable tokens, and current delivery state. `task show`
adds exact Snapshot IDs and the append-only event history. Both commands accept `--json` for a
machine-readable local report. Keep the returned Task ID: it is also the delivery confirmation.

An unverified Task is evidence, not a deliverable. Read `task show`, fix the repository authority,
Gate, Worker, or goal problem, then start a new Task.

## Deliver the verified Snapshot

Choose an absent branch and an absent sibling path. The source checkout must still be clean at
the exact Task source commit.

```sh
pilot_worktree=../client-task-0123456789abcdef0123
pilot_receipt="${pilot_worktree}.delivery.json"
af task deliver task-0123456789abcdef0123 \
  --repo . \
  --branch af/task-0123456789abcdef0123 \
  --worktree "$pilot_worktree" \
  --confirm task-0123456789abcdef0123 \
  --json > "$pilot_receipt"
```

Inspect `ignored_paths` in the JSON delivery receipt before testing creates more files. These are
losslessly percent-encoded verified Snapshot paths that ordinary `git add` omits under the
operator's repository and global ignore rules. They are identifiers, not literal pathspecs. Decode
their filesystem bytes, then prefix Git's literal pathspec magic so names containing glob or
pathspec-magic characters still identify only themselves. Stage them before testing creates more
files:

```sh
python3 - "$pilot_receipt" "$pilot_worktree" <<'PY'
import json
import os
import subprocess
import sys
import urllib.parse

with open(sys.argv[1], encoding="utf-8") as receipt_file:
    encoded = json.load(receipt_file)["ignored_paths"]
paths = [urllib.parse.unquote_to_bytes(path) for path in encoded]
if paths:
    pathspecs = [b":(literal)" + path for path in paths]
    subprocess.run(
        [b"git", b"-C", os.fsencode(sys.argv[2]), b"add", b"-f", b"--", *pathspecs],
        check=True,
    )
PY
```

Alternatively, explicitly accept that the resulting commit differs from the verified Snapshot.
Then inspect and test the delivered worktree. It is deliberately uncommitted. Committing, pushing,
and opening a pull request remain separate human actions using the client's normal Git controls.

An exact repeat is safe. Before sealing, it verifies exact bytes or recovers only an unchanged
owned branch/worktree. After sealing, it verifies the durable delivery identity and returns the
same receipt even if the operator has since built, edited, or committed; that receipt attests to
the original delivery, not the worktree's current bytes. If the process stopped after
preparation, rerun the exact command. If recovery cannot prove that partial content is
delivery-owned, it records a terminal failure and preserves the original branch/worktree. Retry
the same explicitly confirmed Task with a new absent branch and worktree; the next attempt releases
only Afactory's internal ownership ref and never removes the preserved target. Operator changes to
an unsealed worktree are therefore not deleted.

## Troubleshooting

| Symptom | Meaning and action |
|---|---|
| `only a verified Task can be delivered` | Inspect the Task's Gates/evaluator and start a corrected Task. |
| `HEAD no longer equals the Task source revision` | Return the source checkout to the recorded commit or start a Task from the new commit. |
| `worktree is not clean` or `staged changes` | Preserve or commit the operator's work elsewhere, then retry from an exact clean source. |
| branch/path already exists | Choose a new absent target; Afactory never overwrites either one. |
| incomplete rollback/recovery | Rerun the exact command once. If it reports a preserved terminal failure, retain the original branch/worktree and retry the confirmed Task with a new absent target. |
| Worker authentication failure | Re-establish the named machine-local Provider login and confirm with `af provider status`. |
| missing Gate tool | Install the repository-approved toolchain; never remove or weaken the Gate. |
| state or disk error | Preserve the external Task state directory, restore disk capacity/permissions, and retry the exact inspection or delivery command. |

## Pilot release gate

Before handing a build to a design partner, all of the following are required:

- `make check` and `make pilot-check` pass on the exact candidate commit;
- the milestone's pinned external correctness Campaign returns `Pass`;
- the candidate is exercised in a separate trusted repository with its real acceptance Gate;
- install, update, exact-repeat recovery, and binary rollback are rehearsed;
- the human publishes a checksummed private release and records the tag/commit supplied to the
  partner.
