# Keep a manual bugfix workflow alive through merge

The live bugfix/easy workflow can use `scripts/scenario-merge-watch.py` after the owner has
published its exact reviewed candidate. This supported local coordinator keeps running after CI
turns green, observes a later human merge, removes explicitly named clean linked worktrees and
local branches, and writes the caller's final notification. It spends no model tokens and installs
no scheduler. Native Scenario definitions, Wait behavior, journals and adapter contracts stay
unchanged; this script does not drive or complete a native Scenario Run.

## Prepare and launch

Finish implementation and independent verification Tasks first. Use the owner-approved local
Mac gate, `scripts/verify.sh --profile fast`, for this repository. Task acceptance, local Gate
receipts, PR checks and merge are separate evidence. The watcher inspects PR checks; it does not
execute a gate or select a remote gate. Publication and merge remain the human's operations.

Use Python 3.9 or newer, Git, and an already authenticated `gh` on the machine. All GitHub requests
are fixed `gh api --hostname github.com --method GET` inspections. No credentials are created,
read into the journal, or accepted as configuration. No merge, push, comment, issue creation,
remote branch deletion, arbitrary command or shell callback is available.

From a stable coordinator checkout that will remain present, inspect the PR identity and exact
head (replace the repository and PR below with the published ones):

```sh
gh api --hostname github.com --method GET repos/OWNER/REPOSITORY/pulls/593 \
  --jq '{number, head: .head.sha, repository: .base.repo.full_name, repository_id: .base.repo.id}'
git worktree list --porcelain
git rev-parse refs/heads/fix-593
git rev-parse refs/heads/verify-593
```

The exact PR head commit and every cleanup tip must already exist in the coordinator's Git object
database. Cleanup tips must be ancestors of that head, including the head itself. Use only same
repository PRs; fork heads are conservatively refused. Keep all Tasks and other writers stopped
in the named worktrees during watching and cleanup. Do not reuse the named paths or branches
until the watcher terminates. The script coordinates other watchers, not unrelated Git writers.

Create an owner-only JSON file **outside every checkout**, using canonical absolute paths
(`pwd -P` on macOS avoids `/var` and `/private/var` spelling differences). Replace every example
path and SHA with the inspected values. `head` in each cleanup item is that item's exact tip;
a verification branch may point to an earlier ancestor. `required_checks` contains exact GitHub
check-run names, with one unambiguous successful run per name on the pinned head:

```json
{
  "schema": "af/merge-watch-config@1",
  "repository": "OWNER/REPOSITORY",
  "repository_id": 123456,
  "pr": 593,
  "head": "1111111111111111111111111111111111111111",
  "local_repo": "/absolute/coordinator-checkout",
  "state_dir": "/absolute/private-merge-watch-593",
  "required_checks": ["CI"],
  "worktrees": [
    {
      "path": "/absolute/task-worktree-593",
      "branch": "fix-593",
      "head": "1111111111111111111111111111111111111111"
    }
  ],
  "branches": [
    {"branch": "fix-593", "head": "1111111111111111111111111111111111111111"},
    {"branch": "verify-593", "head": "2222222222222222222222222222222222222222"}
  ],
  "poll_seconds": 60,
  "max_seconds": 86400,
  "max_polls": 1440,
  "max_errors": 3
}
```

Name each implementation and verification worktree separately in `worktrees`, and each local
branch separately in `branches`. Neither list implies any other cleanup. At least one target is
required. A checked-out branch cannot be deleted; its linked worktree must be listed for removal
first. The main checkout, locked worktrees, submodules, symlink paths, missing initial targets,
changed tips and worktrees belonging to another clone are refused at admission or cleanup.

```sh
chmod 600 /absolute/merge-watch-593.json
python3 -I scripts/scenario-merge-watch.py --config /absolute/merge-watch-593.json --background
cat /absolute/private-merge-watch-593/status.json
```

`--background` starts a detached process and returns after admission, with its versioned status
JSON and PID. It closes inherited terminal streams; closing the caller does not stop the watcher.
It can immediately reach a terminal state if the PR is already merged or refused. Omit
`--background` to run in the foreground and print the terminal status when finished. Startup
refusals exit 1 with a bounded diagnostic; foreground `FAILED` exits 1 and `DONE` exits 0.
Background launch success means admission, not completion.

## Caller output and safe cleanup

`state_dir/status.json` is both the durable journal and the caller notification destination.
The watcher replaces and fsyncs it atomically. Arrange for the caller to retain this path before
returning control; the coordinator writes its final report there without another prompt or
manual repoll. This is a local file notification, not a chat message, email or arbitrary hook.
The caller may read it at any time. Its `schema` is `af/merge-watch-status@1`
([config schema](../schemas/merge-watch-config-v1.json),
[status schema](../schemas/merge-watch-status-v1.json)).

| Status | Meaning |
|---|---|
| `STARTING` | Exact local bindings admitted; observation is starting. |
| `WAITING_FOR_GITHUB` | Observation unavailable; a bounded retry remains. |
| `WAITING_FOR_CI` | Exact-head checks are missing, ambiguous, incomplete or unsuccessful. |
| `WAITING_FOR_HUMAN_MERGE` | Required CI is green; the same process remains alive waiting for the human. |
| `CLEANING` | Exact human merge verified; cleanup remains, so this is not success. |
| `DONE` | Merge and every configured worktree/branch removal verified; final caller report is in `message`. |
| `FAILED` | Terminal refusal or exhausted budget; inspect `message` and per-target `cleanup` progress. |

GitHub reads bind repository ID/name, PR number and the exact head. CI reads use that commit,
never a movable branch. Checks must fit a complete first page of at most 100 runs; legacy commit
statuses are not check runs. Unsupported, ambiguous or oversized evidence never proves green.
Green CI never proves merge or `DONE`. Merge never substitutes for required CI: even a PR already
merged at startup, or merged during a poll, waits in `WAITING_FOR_CI` until every configured check
succeeds on the exact head. Cleanup and `DONE` require both proofs, including after restart.
Closed-unmerged PRs and head changes fail terminally.

After merge, an immutable commit read must match the reported merge SHA. A normal merge must
include the exact head as a direct parent (or be that commit). A squash merge must have one parent
and exactly the pinned head's Git tree. A squash over a changed base, rebase merge, unavailable
commit or tree mismatch is conservatively refused when neither proof holds. The PR is read again
before cleanup to verify the same merged identity and head. No fetch or remote mutation is needed.

For each worktree, the watcher checks its linked administrative entry, original directory identity,
branch, tip, index, raw file bytes and executable/symlink modes. Filters, hooks and fsmonitor are
disabled, and submodules are refused. Untracked and ignored files, including `target/`, count as
dirty: arrange separate build artifact cleanup before launching, while preserving audit data.
A tree containing `events.sqlite` is refused even if that file is tracked. Removal uses Git without
`--force`. Branch deletion uses an exact old-tip comparison and refuses any branch still checked
out. Task Stores, audit evidence and remote branches stay retained. No `af task gc`, storage prune,
workspace unregistration or recursive Store deletion runs; Workspace registry entries stay retained.

Cleanup order is the configured worktrees, then the configured branches. Failure can leave an
earlier target removed and later targets intact; `cleanup` records exactly which steps completed.
`DONE` requires every target's absence to be read back, including the worktree administrative
listing. No message claims rollback.

## Bounds, restart and intervention

Polling interval is 1–3600 seconds, runtime at most seven days, total polls at most 20,000, and
cumulative observation errors at most eight. Each command has at most 15 seconds within the
remaining wall budget, at most 1 MiB of captured output, and a private process group terminated
on completion or refusal. Cleanup scans allow at most 20,000 entries and 256 MiB of regular file
content per worktree, checking the wall deadline while hashing. Configuration has at most sixteen
worktrees, sixteen branches and thirty-two required check names. Journal bytes are bounded at
1 MiB; repeated polling replaces one file rather than appending logs.

Restart the same command and unchanged configuration after a process loss. The original absolute
deadline, cumulative poll/error counts, merge evidence and cleanup intents remain binding; restart
does not renew a budget. Unknown versions, corruption and changed bindings fail closed. Private
advisory locks refuse concurrent watchers for the same state directory and for the same repository
and PR even with a different state directory. Lock files are retained so their inode stays stable.

A cleanup intent is fsynced before deletion. Restart reconciles positive absence as completed.
If a target with an uncertain intent still exists, the watcher fails and preserves it rather than
repeating a potentially completed deletion. Partial administrative removal, replacements at
completed target names, changed identities and changed tips also fail. Inspect these manually;
never edit a journal to manufacture `DONE`. Terminal replay returns the retained result and
performs no cleanup, including when someone has since reused a path or branch.

To stop a watcher early, send SIGTERM to only the PID recorded in its private status after checking
that it is still this process. SIGTERM/SIGINT stop the current command group, leave resumable
progress and exit 130. A stopped watcher can resume under the original budgets. For terminal
failure, finish remaining cleanup manually and retain this failed report; a fresh configuration
and fresh state directory require a new explicit operator decision. No automatic retry resets
terminal failure or contradicts a retained `DONE`.

The credential-free process tests run with
`python3 -I scripts/test-scenario-merge-watch.py`: a fake `gh` enforces read-only argument vectors,
real disposable Git worktrees exercise cleanup, and service rendezvous drives a later human merge.
Only the polling sleep is virtualized in the test executable; no live GitHub operation runs.
