# Remote Checks — design and implementation plan

**Status:** proposed, 2026-10-04. Package RC1 is not yet delivered.
**Vocabulary:** [`CONTEXT.md`](../../CONTEXT.md). **Values:** [`../values.md`](../values.md).

A Task check today is a command this machine runs. This note lets a machine hand a declared
check to a remote executor — a draft pull request and the repository's own CI — and bind the
result to the exact Snapshot, so a host too small to build the project can still hold a real
Gate.

## 1. Problem

On 2026-10-04 an agent host (2 vCPU, 3.8 GB RAM, the gateway alone holding 1.6 GB) was audited
after two weeks of developing this repository through `af`. The numbers below are that host's
own audit of its Task Stores, not re-derived here.

- A cold `make check` takes 43 to 60 minutes there and about 12 minutes on the repository's
  GitHub runner. `.af/code-policy.toml` allows 60 minutes, so the `kernel` check timed out in at
  least four Tasks before any reviewer ran.
- Across the issue #121 Tasks, checks took 16 h 21 min of 19 h of measured Attempt time; model
  work took 2 h 20 min.
- Link steps were killed for memory three times, each time taking the agent host down with every
  running Task.

The agent then did the obvious thing by hand: pushed the candidate, opened a draft pull request
and watched CI go green. `af` could not use that result. Nothing turns a CI conclusion into an
`af/TaskCheckReceipt@1`, so the Task stayed unverified and the agent was told, correctly, that
green CI is not acceptance.

The missing piece is small. The CI run is the same check the policy already names, executed
somewhere else. What the kernel needs is a way to (a) put exactly the checked Snapshot in front
of that executor, (b) wait for it inside the check Attempt, and (c) record evidence that the
conclusion belongs to that Snapshot and no other tree.

## 2. Outcome and fixed requirements

These bind package RC1. A change that cannot meet one of them is not done.

1. **Same check, another executor.** A check in `.af/code-policy.toml` may add a `remote` table.
   Its `command` stays mandatory: every remote check has a local equivalent. A policy without
   the table is captured byte-identically to today and behaves identically.
2. **The machine chooses.** An operator's machine-local mapping names, per repository, which
   declared checks run remotely on this machine. No mapping, or a check not listed in it, means
   the local command runs exactly as today. Committed policy never forces a remote run and
   never grants push authority.
3. **Exact Snapshot.** A remote conclusion is accepted only for a commit whose tree the kernel
   built from the checked Snapshot and read back, on a pull request whose base is an ancestor of
   that commit, so the tree CI merges and tests is the Snapshot's tree. The evidence names the
   Snapshot, the commits, the pull request and every check run it relied on.
4. **Local first.** Within one check Attempt, locally executed checks run first. Remote checks
   are dispatched only when every required local check passed, so a candidate that fails
   formatting never reaches CI.
5. **A narrow publishing boundary.** For a remote check the kernel pushes exactly two branches
   under `af-gate/<task-id>/` and opens one draft pull request between them. It never
   force-pushes, pushes any other ref, merges, marks ready, closes, comments or deletes.
   Delivery is unchanged ([ADR-0031](../adr/0031-deliver-verified-tasks-to-new-local-worktrees.md)):
   it still never commits, pushes or opens a pull request.
6. **No credential in the kernel.** `git` and `gh` run with the operator's ambient
   authentication, as coordinator subprocesses, never inside a check sandbox or a Worker. `af`
   reads, stores and prints no token; a mapping whose push URL carries user information is
   refused.
7. **A Worker cannot write the workflow.** A candidate whose `.github/` tree differs from the
   Task's captured source Snapshot is never sent to a remote executor.
8. **No new budget.** A remote check costs zero tokens. Its preparation and its wait are charged
   to the same check Attempt under the existing `check_wall_ms` and `check_process_wall_ms`.
9. **Could not run is not a pass.** Every refusal and every inconclusive remote state is a
   `NotRun` result with a named reason and fix, and blocks like any required check that could
   not run.
10. **Resume attaches.** The same Task and Snapshot always resolve to the same branches, commit
    and pull request. A resumed or repeated check finds them and waits; it never opens a second
    pull request or pushes a duplicate commit.
11. **Observed once.** Every remote fact the decision used is recorded in one typed artifact.
    Replay reads the artifact and never asks the remote again.

Out of scope for RC1, each a separate decision: Campaign `[gate]` checks of `af review run`;
executors other than GitHub pull requests; closing or collecting gate pull requests; using the
gate pull request as the delivery pull request; remote execution of `[measures]`; the
`execute-checks` shell of review Workers.

## 3. Model

### 3.1 Declaration

```toml
[checks.kernel]
name = "kernel"
required = true

[checks.kernel.command]
program = "bash"
# ... unchanged ...

[checks.kernel.remote]
executor = "github-pr"
required = ["validation / lint", "validation / check (ubuntu-latest)"]
```

- `executor` is a closed set; RC1 admits `github-pr` only. An unknown name is refused when the
  policy is captured.
- `required` lists 1 to 32 distinct check-run names, each 1 to 128 characters, exactly as GitHub
  reports them for a commit. All of them must conclude `success` for the check to pass.
- The table is optional per check. The policy schema stays `af.code-task-policy/1`; the JSON
  schema and the policy validator gain the table.

Declaring both forms is the project's statement that they verify the same thing. The kernel
does not compare a workflow with a command.

### 3.2 Machine mapping

The mapping is an operator file outside any source tree: `$XDG_CONFIG_HOME/af/remote-checks.toml`,
or the absolute path in `AF_TASK_REMOTE_CHECK_POLICY_FILE` when set. The coordinator resolves it
once when it constructs the code domain, as it does for the Rust toolchain mapping; candidate
commands never receive the variable or the path.

```toml
version = 1

[[github_pr]]
repository_id = "5f1c…"            # git rev-list --max-parents=0 HEAD, sorted
github = "PakhomovAlexander/afactory"
push_url = "git@github.com:PakhomovAlexander/afactory.git"
checks = ["kernel"]
```

- `repository_id` is the identity Task source origins already record (the repository's sorted
  root commits), so one machine can map several repositories without ambiguity.
- `checks` names declared checks. A name the captured policy does not declare, or declares
  without a `remote` table, is an error before any check starts.
- `push_url` is any Git URL without user information. `github` is the `owner/name` used for
  API calls through `gh`.
- A malformed selected mapping is an error before the first check starts. An absent file is
  not an error: every check runs locally.

The mapping is the operator's authorization for this machine to push the gate branches of that
repository. It is machine-local for the same reason Provider bindings are: authority to act on
a remote belongs to a person at a machine, not to a commit.

### 3.3 Selection and order within one check Attempt

For the named checks of one Check node:

1. Partition them into *remote-selected* (declared with `remote`, listed in the mapping entry
   for the Snapshot's `repository_id`) and *local* (all others).
2. Run every local check as today, in name order.
3. If any required local check did not pass, record each remote-selected check as `NotRun` with
   reason `remote_skipped_local_failed`. Nothing is pushed.
4. Otherwise run the remote-selected checks (3.4). They share one push and one pull request and
   are judged separately, each against its own `required` names.

A remote-selected check never executes candidate code on this machine: no sandbox is
materialized for it, no Rust toolchain snapshot is prepared and no warm directory is bound.
Under `[warm]` it still has its evidence group, with each declared kind recorded as skipped for
the reason `remote`.

### 3.4 Transport: two branches and one draft pull request

Let *candidate* be the Snapshot under check and *source* the Snapshot reached by following
`parent_snapshot_id` from it until none remains (the Task's captured source; the candidate
itself when it has no parent).

1. **Refuse a Worker-written workflow.** If the manifest entries under `.github/` differ
   between candidate and source in any path, mode or content, stop: `remote_candidate_changes_ci`.
2. **Build commits in a private repository.** In a temporary Git repository the kernel owns
   (never the operator's checkout), write the source tree and the candidate tree from their
   manifests: regular files `100644`, executables `100755`, symbolic links `120000`. Read each
   tree back with `git ls-tree -r` and compare every path, mode and blob identity with the
   manifest before using it. A mismatch is a kernel error, not a check result.
3. **Fixed identities.** The base commit has the source tree, no parent, and fixed metadata
   (author and committer `af <af@localhost>`, time zero UTC, message `af gate base
   <source snapshot id>`). Its identity is a function of the source Snapshot alone.
4. **Branches.** `refs/heads/af-gate/<task-id>/base` and `refs/heads/af-gate/<task-id>/head`.
   A Task ID that is not a valid single ref component is `remote_ref_invalid`.
5. **Reconcile with the remote** (`git ls-remote`, then plain pushes, never `--force`):
   - base absent: push it. Base present with another identity: `remote_ref_conflict`.
   - head absent: the head commit is a child of base with the candidate tree and message
     `af gate <candidate snapshot id>`; push it.
   - head present and its tip has the candidate tree: attach, push nothing.
   - head present with another tree: the head commit is a child of that tip with the candidate
     tree; push it as a fast-forward. Each repair round is therefore one more commit on the
     same pull request.
   A rejected push is `remote_push_refused` with Git's bounded diagnostic.
6. **Pull request.** Find the open pull request from head to base; when none exists, open one
   as a draft with a fixed title (`af gate: <task-id>`) and a fixed body saying it was opened by
   a machine to run declared checks and is not for review or merge. A failure is
   `remote_pr_refused`.
7. **Wait.** Read the check runs of the head commit immediately and then every 15 seconds until
   every `required` name of every remote-selected check is present and completed, the Attempt's
   remaining wall is spent, or the Attempt is cancelled. A required name that has not appeared
   10 minutes after the push is `remote_check_missing`: the name is wrong, or the workflow does
   not run for draft pull requests into `af-gate/**` bases.
8. **Judge each check.** All required names `success`: `Passed`. Any required name `failure`:
   `Failed`. Any other conclusion (`cancelled`, `skipped`, `timed_out`, `neutral`,
   `action_required`, `stale`, `startup_failure`): `NotRun` with `remote_check_inconclusive`
   naming the check run and its conclusion. Deadline or cancellation while waiting: `NotRun`
   with the same deadline reason a local check uses.
9. **Keep what a failure said.** For each failed required check run, fetch its job log and keep
   the last 256 KiB (1 MiB across the check) as the check's `stdout` artifact, so a repair
   Worker and a reviewer read a remote failure where they read a local one. A log that cannot
   be fetched changes no outcome; the evidence says `logs: unavailable`.

Because base is an ancestor of every head commit, the merge GitHub builds for a `pull_request`
run has exactly the head commit's tree. Because base and head are commits the kernel made from
Snapshots, nothing a person has not already chosen to hand to this Task is published: no local
branch, no local history.

Every `git` and `gh` call is a supervised subprocess with `GIT_TERMINAL_PROMPT=0` and
`GH_PROMPT_DISABLED=1`, its own bounded wall inside the Attempt's remaining time, and the
shared kill path on cancellation. `git` or `gh` missing, or `gh` unauthenticated, is
`remote_tool_unavailable` naming the fix.

### 3.5 Evidence

One artifact per remote-selected check, `af/RemoteCheckEvidence@1`:

| Field | Meaning |
| --- | --- |
| `executor` | `github-pr` |
| `github` | `owner/name` |
| `pull_request` | number |
| `snapshot_id`, `source_snapshot_id` | what was checked and what it derives from |
| `base_commit`, `head_commit`, `tree` | Git identities the kernel built and pushed |
| `checks[]` | per required name: check run ID, conclusion, started and completed times, URL |
| `logs` | `kept`, `none` or `unavailable` |
| `observed_unix_ms` | when the deciding observation was read |

`CheckResult` gains one optional field, `remote`, holding that artifact's ID; a local result
omits it and serializes as today. A remote result has no `program` and no `exit_code`. The
receipt type and its meaning do not change: `af/TaskCheckReceipt@1` still maps check names to
result artifacts for one plan, Snapshot and policy.

`af task show` prints, for a remote check, the executor, the pull request URL, each required
check run with its conclusion and duration, and the refusal reason when there is one. `--json`
carries the evidence document. Neither prints the push URL or any mapping path.

### 3.6 Refusal reasons

| Reason | Cause | Fix the message names |
| --- | --- | --- |
| `remote_skipped_local_failed` | a required local check did not pass | fix the local failure |
| `remote_candidate_changes_ci` | candidate differs from source under `.github/` | run this Task where the check is local |
| `remote_tool_unavailable` | `git`/`gh` missing, or `gh` not authenticated | install or `gh auth login` |
| `remote_ref_invalid` | Task ID is not a ref component | use a ref-safe Task ID |
| `remote_ref_conflict` | the base branch exists with another identity | another Task owns the name; delete it or rename the Task |
| `remote_push_refused` | the remote rejected a push | Git's diagnostic (ruleset, permission) |
| `remote_pr_refused` | the pull request could not be opened | `gh`'s diagnostic |
| `remote_check_missing` | a required name never appeared | correct the name or the workflow trigger |
| `remote_check_inconclusive` | a required check run ended without success or failure | rerun it on GitHub, then resume |

### 3.7 What stays behind

The two branches and the draft pull request outlive the check: later repair rounds and a
resumed Task use them. RC1 never removes them. `af task show` prints the two commands a person
runs to close the pull request and delete the branches. Collecting them with `af task gc` is a
follow-up.

### 3.8 Where this stops

- CI executes candidate code with whatever permissions and secrets the repository's workflows
  give a same-repository pull request. The mapping is the operator's statement that this is
  acceptable for that repository; the `.github/` rule keeps a Worker from changing what those
  workflows are. Point a remote check only at workflows you would run for a collaborator's
  branch.
- The evidence proves which commit GitHub reported on, not what the runner did. A compromised
  runner or account is outside the model, as a compromised local toolchain is today.
- Two machines running different Tasks under one Task ID against one repository collide on the
  base branch and are refused, never merged.

## 4. Packages

### RC1 — Remote checks in the kernel

**Depends on:** nothing.

Deliverables:

1. `CheckDefinition` gains the optional `remote` table of 3.1, with validation at capture, the
   JSON schema (`schemas/code-task-policy-v1.json`) and its parity fixture. A policy without
   the table has the same captured bytes and policy identity as before.
2. The machine mapping of 3.2: the default path, `AF_TASK_REMOTE_CHECK_POLICY_FILE`, bounded
   no-follow reading as for the Rust toolchain mapping, validation against the captured policy,
   and selection by the Snapshot origin's `repository_id`.
3. The selection and local-first order of 3.3 inside the existing code check operator,
   including the `[warm]` evidence group of a remote-selected check.
4. The `github-pr` executor of 3.4 behind one internal seam (trait or module boundary) that the
   check operator calls with the candidate Snapshot, the deadline and the cancellation flag:
   the `.github/` refusal, private-repository commit construction with read-back, fixed
   identities, branch reconciliation without force, the draft pull request, the bounded wait,
   per-check judgement and the failed-log excerpt. All `git`/`gh` calls go through the shared
   process supervision.
5. `af/RemoteCheckEvidence@1` with its schema and fixture, the optional `remote` field on
   `CheckResult` (`schemas/check-result-v1.json`), and the `af task show` text and `--json`
   rendering of 3.5 and 3.7.
6. Every reason of 3.6 as a `NotRun` result whose message names the entity, the knob and the
   fix.
7. Tests, all credential-free and offline: a real `git` pushing to a local bare repository as
   `push_url`, and a fake `gh` executable on `PATH` that serves recorded API documents. They
   cover: no mapping (local run, unchanged bytes); pass; fail with log excerpt; each reason of
   3.6; local failure skips the push; resume attaches without a second pull request or commit;
   a repair round appends one commit; a candidate changing `.github/` is refused before any
   push; a mapping with user information in `push_url` is refused; cancellation during the
   wait ends the subprocesses.
8. One ADR (0136) recording the decision, the four choices made on 2026-10-04 (draft pull
   request over a bare gate branch; the kernel pushes under an operator mapping over a
   host-pushed commit; per-machine selection over policy-fixed; refusing `.github/` changes
   over flagging them) with the rejected options, and the amendment to the publishing
   invariant. `AGENTS.md` gains the invariant of §2.5 beside the delivery one; `CONTEXT.md`
   gains *Remote Check*, *Remote Check mapping* and *Gate pull request*;
   `docs/task-execution/remote-checks.md` is the walkthrough (declare, map, run, read the
   evidence, clean up, what a workflow must allow); `docs/non-goals.md` keeps the delivery
   non-goal and points at the ADR; `changelog.d/remote-checks.md` carries the release note.

Acceptance:

- With no mapping, a Task over a policy that declares `remote` produces the same check results
  and receipt as the same policy without the table, apart from the policy identity.
- With a mapping, a passing fixture records `Passed` with evidence naming the pull request, both
  commits and each required check run, and the pushed head commit's tree equals the candidate
  manifest entry for entry.
- A fixture whose local `fmt` check fails pushes nothing and records the remote check `NotRun`
  with `remote_skipped_local_failed`.
- Running the same check Attempt twice against the same remote state creates no second pull
  request and no second commit.
- No test, fixture or recorded artifact contains a credential, the push URL or a mapping path.
- No `git push` invocation in the implementation carries `--force` or a `+` refspec, and no ref
  outside `refs/heads/af-gate/<task-id>/` is ever written.

### RC2 — Adoption and live proof (no package code)

By hand, after RC1 is delivered and released into a build this repository can run:

1. Read the real check-run names from a recent pull request of this repository and add
   `[checks.kernel.remote]` to `.af/code-policy.toml`.
2. Write the mapping on the agent host and run one real Task there: the `kernel` check remote,
   `markdownlint` local.
3. Record in §6: the remote check's wall time on that host against its last local gate, the
   push size and time from a private repository, and anything the live run contradicted in
   this note.

## 5. Validation and rollback discipline

- Package RC1 is implemented by an `af` Task (`kernel/implementation-reviewed`) from this
  repository's own policy and verified by its reviewers and evaluator. Findings are fixed by
  hand in the delivery worktree and re-verified at most once.
- Nothing in RC1 runs against a real remote. The only live step is RC2, started by a person.
- Rollback is deleting the mapping file: every check runs locally again, with no state to
  migrate.

## 6. Execution record

### Design review

Not yet run.

### Packages

Not yet started.
