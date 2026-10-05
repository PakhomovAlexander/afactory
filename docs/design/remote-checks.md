# Remote Checks — design and implementation plan

**Status:** proposed, 2026-10-04; revised the same day after the design review recorded in §6.
Package RC1 is implemented in the change that adds
[ADR-0139](../adr/0139-run-a-declared-check-through-a-gate-pull-request.md); RC2 is not started.
**Vocabulary:** [`CONTEXT.md`](../../CONTEXT.md). **Values:** [`../values.md`](../values.md).

A Task check today is a command this machine runs. This note lets a machine hand a declared
check to a remote executor — a draft pull request and the repository's own GitHub Actions
workflow — and bind the result to the exact Snapshot, so a host too small to build the project
can still hold a real Gate.

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
3. **Exact Snapshot, verified.** A remote conclusion is accepted only when the kernel has read
   back, from the remote, that the tree GitHub merged and tested for the pull request equals the
   tree it built from the checked Snapshot, and that every job it relied on belongs to the
   declared workflow's `pull_request` run for that pull request and head commit. The evidence
   names the Snapshot, the commits, the pull request, the run and every job.
4. **Local first.** Within one check Attempt, locally executed checks run first. Remote checks
   are dispatched only when every required local check passed, so a candidate that fails
   formatting never reaches CI.
5. **A narrow, explicit exception to "humans publish".** For a remote check the kernel pushes
   exactly two branches under `af-gate/<task-id>/` and opens one draft pull request between
   them. It never force-pushes, pushes any other ref, merges, marks ready, closes, comments or
   deletes. This is a new, operator-authorized exception to the rule that publishing is a human
   action; RC1 must state it where that rule is stated (`docs/values.md`, `AGENTS.md`,
   ADR-0139) before the behaviour exists. Delivery is unchanged
   ([ADR-0031](../adr/0031-deliver-verified-tasks-to-new-local-worktrees.md)): it still never
   commits, pushes or opens a pull request.
6. **No credential and no machine path in any record.** `git` and `gh` run with the operator's
   ambient authentication, as coordinator subprocesses, never inside a check sandbox or a
   Worker. `af` reads, stores and prints no token; a mapping whose push URL carries user
   information is refused. No artifact, event or command output contains the push URL, the
   mapping path, or a raw remote diagnostic that was not redacted for both. A failed job's log
   excerpt is kept for debugging (3.4 step 10, 3.8).
7. **A Worker cannot write the workflow.** A candidate whose `.github/` tree differs from the
   Task's captured source Snapshot is never sent to a remote executor.
8. **No new budget.** A remote check costs zero tokens. Its preparation and its wait are charged
   to the same check Attempt under the existing `check_wall_ms` and `check_process_wall_ms`
   (3.3 says exactly how).
9. **Could not run is not a pass.** Every refusal and every inconclusive remote state is a
   `NotRun` result with a named reason and fix, and blocks like any required check that could
   not run.
10. **Resume attaches, and only to its own.** The same Task and Snapshot always resolve to the
    same branches, commit and pull request. A resumed or repeated check verifies that the
    branches it finds were made by this Task from this source before it waits on them; it never
    opens a second pull request, pushes a duplicate commit, or adopts another Task's branch.
11. **Observed once.** Every remote fact the decision used is recorded in one typed artifact.
    Replay reads the artifact and never asks the remote again.

Out of scope for RC1, each a separate decision: Campaign `[gate]` checks of `af review run`;
executors other than GitHub Actions pull-request runs; check runs posted by other GitHub apps;
closing or collecting gate pull requests; using the gate pull request as the delivery pull
request; remote execution of `[measures]`; the `execute-checks` shell of
review Workers.

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
workflow = ".github/workflows/ci.yml"
required = ["validation / lint", "validation / check (ubuntu-latest)"]
```

- `executor` is a closed set; RC1 admits `github-pr` only. An unknown name is refused when the
  policy is captured.
- `workflow` is the repository path of the workflow whose `pull_request` run earns the result.
  Jobs of any other workflow are never admitted, whatever their names.
- `required` lists 1 to 32 distinct job names, each 1 to 128 characters, exactly as the GitHub
  Actions jobs API reports them for that run (a job of a called workflow reads
  `validation / lint`). All of them must conclude `success` for the check to pass.
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

### 3.3 Selection, order and time within one check Attempt

For the named checks of one Check node:

1. Partition them into *remote-selected* (declared with `remote`, listed in the mapping entry
   for the Snapshot's `repository_id`) and *local* (all others).
2. Run every local check as today, in name order, each under its own
   `check_process_wall_ms` as today.
3. If any required local check did not pass, record each remote-selected check as `NotRun` with
   reason `remote_skipped_local_failed`. Nothing is pushed.
4. Otherwise run the remote phase (3.4) once for all remote-selected checks. They share one
   push and one pull request and are judged separately, each against its own `workflow` and
   `required` names.

**One clock for the remote phase.** The remote phase starts one timer when it begins (before
the `.github/` comparison). Transport and polling time is charged to every remote-selected
check alike. The phase ends at the earlier of `check_process_wall_ms` on that timer and the
Attempt's deadline. A check is judged as soon as all its required jobs are complete; a check
still incomplete when the phase ends is `NotRun` with the deadline reason a local check uses.
No remote-selected check can therefore pass on an observation made after its captured
per-check limit.

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
3. **Fixed identities with an owner.** Every gate commit has fixed metadata (author and
   committer `af <af@localhost>`, time zero UTC) and a message the kernel can parse back. The
   base commit has the source tree, no parent, and a message naming the source Snapshot, the
   Task ID and the Task's durable identity in its Store: a digest of the transition that opened
   the Task's log, whose writer carries 64 bits from the operating system's random source
   (ADR-0139; a first revision's ID is content-addressed and equal across Stores). Its identity
   is thus a function of the source Snapshot and the owning Task,
   and of nothing else.
4. **Branches.** `refs/heads/af-gate/<task-id>/base` and `refs/heads/af-gate/<task-id>/head`.
   A Task ID that is not a valid single ref component is `remote_ref_invalid`.
5. **Reconcile with the remote** (`git ls-remote`, fetches into the private repository, then
   plain pushes, never `--force` and never a `+` refspec):
   - base absent: push it. Base present with another identity: `remote_ref_conflict` — another
     Task, or this Task ID in another Store, owns the name.
   - head absent: the head commit is a child of base with the candidate tree and a message
     naming the candidate Snapshot; push it.
   - head present: fetch it and verify the whole chain before trusting it. Walking from its tip,
     every commit has exactly one parent and a gate-commit message, and the walk ends at exactly
     this Task's base commit. Each of those commits must also name a Snapshot this Task's Store
     holds and carry exactly that Snapshot's tree. Anything else is `remote_ref_conflict`. Then,
     if the tip names the candidate Snapshot and has its tree, attach and push nothing;
     otherwise the head commit is a child of that tip with the candidate tree, pushed as a
     fast-forward. Each repair round is therefore one
     more commit on the same pull request.
   A rejected push is `remote_push_refused`. A push stopped by the deadline or by cancellation
   proves nothing, because the remote may have accepted it first: read the two branches back
   through one recovery call with its own short bound, which may run after the deadline or the
   cancellation and never judges. Both as pushed is a landed push (go on with time left,
   otherwise record `published`); both as found is a refusal; unreadable ends the Attempt with a
   kernel error and no evidence, for resume to reconcile.
6. **Pull request.** Look up the pull request from head to base in `github`, in every state.
   Attach to an open one. When none exists, open one as a draft with a fixed title
   (`af gate: <task-id>`) and a fixed body saying it was opened by a machine to run declared
   checks and is not for review or merge. A closed one is never replaced by a second: the check
   is `remote_pr_refused`, naming the pull request to reopen. Any other failure is also
   `remote_pr_refused`.
7. **Wait on the right run.** Immediately and then every 15 seconds, until every remote-selected
   check is judged or the remote phase ends (3.3) or the Attempt is cancelled:
   - list the Actions workflow runs with event `pull_request` and head SHA equal to the head
     commit; keep those whose workflow path is a declared `workflow` and whose pull-request list
     contains this pull request number;
   - for each kept run, read its jobs for the latest attempt only.

   Zero kept runs for a declared workflow 10 minutes after the push is `remote_check_missing`
   (the workflow does not run for draft pull requests into `af-gate/**` bases). More than one
   kept run for one workflow is `remote_check_ambiguous`. A completed run whose latest attempt
   lacks a required job name is `remote_check_missing`; a job name appearing twice in it is
   `remote_check_ambiguous`. A listing the reader's page limit does not exhaust judges nothing.
8. **Prove what was tested.** Before any check is judged `Passed` or `Failed`, read the pull
   request and its merge ref from the remote and require: the pull request is open, in `github`,
   from this head branch to this base branch, with head SHA the head commit and base SHA the
   base commit; and `refs/pull/<n>/merge` is a commit whose parents are that base and that head
   and whose tree equals the candidate tree. A stale merge ref is re-read until the phase ends.
   Failing this is `remote_merge_mismatch`. The proof is read again for every batch of checks
   about to be judged; one read for an earlier check never carries a later one.
9. **Judge each check.** All required jobs `success`: `Passed`. Any required job `failure`:
   `Failed`. Any other conclusion (`cancelled`, `skipped`, `timed_out`, `neutral`,
   `action_required`, `stale`, `startup_failure`): `NotRun` with `remote_check_inconclusive`
   naming the job and its conclusion. A person may rerun jobs on GitHub and resume the Task: the
   next observation reads the run's new latest attempt and records its number.
10. **Keep what a failure said.** For each required job that did not succeed, record the names
    and conclusions of its steps that did not succeed (from the jobs API) and the job URL, and
    fetch the job's log through the jobs API. Keep the last 256 KiB of each such log, at most
    1 MiB across the check, each under a header line naming its job, as the check's `stdout`
    artifact. A repair Worker, a reviewer and a person debugging then read a remote failure
    where they read a local one. A log that cannot be fetched is left out and changes no outcome.

Because base and head are commits the kernel made from Snapshots, nothing a person has not
already chosen to hand to this Task is published: no local branch, no local history.

Every `git` and `gh` call is a supervised subprocess with `GIT_TERMINAL_PROMPT=0` and
`GH_PROMPT_DISABLED=1`, its own bounded wall inside the phase's remaining time, and the shared
kill path on cancellation. `git` or `gh` missing, or `gh` unauthenticated, is
`remote_tool_unavailable` naming the fix.

**Diagnostics are redacted before they exist anywhere.** A refusal may keep at most 2 KiB of a
subprocess's diagnostic. Before it is written to an artifact, an event or any output, every
occurrence of the exact push URL is replaced by `<push-url>` and of the mapping path by
`<mapping>`. The private repository's path is likewise replaced by `<gate-repository>`.

### 3.5 Evidence and the result contract

One artifact per remote-selected check, `af/RemoteCheckEvidence@1`, tagged by `state`:

| State | When | Fields beyond the common ones |
| --- | --- | --- |
| `refused` | nothing was written to the remote for this check | `reason`, optional redacted `diagnostic` |
| `published` | branches (and perhaps the pull request) exist, no job decided the check | `reason`, optional `diagnostic`, `base_commit`, `head_commit`, `tree`, optional `pull_request` |
| `observed` | jobs decided the check: passed, failed or inconclusive | `base_commit`, `head_commit`, `tree`, `pull_request`, `merge_commit`, `run` (ID, attempt, workflow path), `jobs[]`, optional `reason` |

Common fields: `executor`, `github`, `workflow`, `required`, `snapshot_id`,
`source_snapshot_id`, `observed_unix_ms`. Each `jobs[]` entry has the job ID, name, conclusion,
started and completed times, URL and, when it did not succeed, up to 32 unsuccessful steps as
name and conclusion (names bounded to 128 characters). The schema fixes which fields each state
requires and forbids, and every text bound.

**`CheckResult` gains a second, distinct shape.** A local result is unchanged and serializes as
today. A remote result carries `remote` (the evidence artifact ID) and has no `program`, no
`exit_code`, no `stderr`, and empty `args`. Its `stdout`, present only when the check did not
pass, is the log excerpt of 3.4 step 10: an attachment for whoever debugs the failure, not
evidence, and no decision reads it. `schemas/check-result-v1.json` states both shapes and
refuses a mixture.

**The reader validates a remote result instead of comparing a command.** Where
`check_outcome` today requires a result's program and arguments to equal the captured command,
for a result carrying `remote` it requires instead that: the captured definition declares
`remote`; the evidence artifact validates; its `executor`, `workflow` and `required` equal the
definition's; its `snapshot_id` is the receipt's Snapshot; and the result's status is the one
the evidence derives (`observed` with every required job `success` is `passed`; `observed` with
a required job `failure` is `failed`; everything else is `not_run` with the evidence's reason),
and that a `stdout` artifact, when present, verifies and accompanies `observed` evidence.
A remote result for a definition without `remote`, or a status the evidence does not derive, is
refused like a result that changed its captured definition. The receipt type and its meaning do
not change: `af/TaskCheckReceipt@1` still maps check names to result artifacts for one plan,
Snapshot and policy.

`af task show` prints, for a remote check, the executor, the pull request URL, the run and its
attempt, each required job with its conclusion and duration, the unsuccessful steps, whether a
log excerpt was kept, and the refusal reason when there is one. `--json` carries the evidence
document.

### 3.6 Refusal reasons

| Reason | Cause | Fix the message names |
| --- | --- | --- |
| `remote_skipped_local_failed` | a required local check did not pass | fix the local failure |
| `remote_candidate_changes_ci` | candidate differs from source under `.github/` | run this Task where the check is local |
| `remote_tool_unavailable` | `git`/`gh` missing, or `gh` not authenticated | install or `gh auth login` |
| `remote_ref_invalid` | Task ID is not a ref component | use a ref-safe Task ID |
| `remote_ref_conflict` | base has another identity, or head is not this Task's chain | another Task owns the name; delete the branches or rename the Task |
| `remote_push_refused` | the remote rejected a push | the redacted diagnostic (ruleset, permission) |
| `remote_pr_refused` | the pull request could not be opened | the redacted diagnostic |
| `remote_check_missing` | no run of the workflow, or a required job absent from it | correct the name, the workflow path or its trigger |
| `remote_check_ambiguous` | two runs of one workflow, or a duplicated job name | make job names unique; cancel the stray run |
| `remote_merge_mismatch` | the pull request or its merge ref is not what the kernel pushed | someone changed the gate pull request; restore or delete it |
| `remote_check_inconclusive` | a required job ended without success or failure | rerun it on GitHub, then resume |

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
- The evidence proves which run and merge commit GitHub reported, not what the runner did. A
  compromised runner or account is outside the model, as a compromised local toolchain is
  today.
- A job log excerpt is stored as GitHub served it. GitHub masks the repository's registered
  secrets in job logs; a workflow that prints one in another form would put it in the Store, as
  it already puts it in the pull request's log, which every collaborator can read. Decided by
  the owner on 2026-10-04: debugging a remote failure needs the log, and withholding it was
  more protection than this boundary warrants. ADR-0139 records the accepted exposure.

## 4. Packages

### RC1 — Remote checks in the kernel

**Depends on:** nothing.

Deliverables:

1. `CheckDefinition` gains the optional `remote` table of 3.1 (`executor`, `workflow`,
   `required`), with validation at capture, the JSON schema
   (`schemas/code-task-policy-v1.json`) and its parity fixture. A policy without the table has
   the same captured bytes and policy identity as before.
2. The machine mapping of 3.2: the default path, `AF_TASK_REMOTE_CHECK_POLICY_FILE`, bounded
   no-follow reading as for the Rust toolchain mapping, validation against the captured policy,
   and selection by the Snapshot origin's `repository_id`.
3. The selection, local-first order and single remote-phase clock of 3.3 inside the existing
   code check operator, including the `[warm]` evidence group of a remote-selected check.
4. The `github-pr` executor of 3.4 behind one internal seam (trait or module boundary) that the
   check operator calls with the candidate Snapshot, the phase deadline and the cancellation
   flag: the `.github/` refusal; private-repository commit construction with read-back; owned,
   parseable gate commits; branch reconciliation with full chain verification and no force; the
   draft pull request; the wait on `pull_request` runs of the declared workflow for this pull
   request and head commit, latest attempt only; the pull-request and merge-ref proof; per-check
   judgement; unsuccessful step names and the bounded log excerpts. All `git`/`gh` calls go
   through the shared process supervision, and every kept diagnostic goes through the redaction
   of 3.4.
5. `af/RemoteCheckEvidence@1` with its three states, schema and fixtures; the second
   `CheckResult` shape in `schemas/check-result-v1.json`; the remote branch of `check_outcome`
   as specified in 3.5; and the `af task show` text and `--json` rendering of 3.5 and 3.7.
6. Every reason of 3.6 as a `NotRun` result whose message names the entity, the knob and the
   fix.
7. Tests, all credential-free and offline: a real `git` pushing to a local bare repository as
   `push_url`, and a fake `gh` executable on `PATH` that serves recorded API documents. They
   cover: no mapping (local run, unchanged bytes); pass; fail with unsuccessful step names and
   the failed jobs' log tails kept as `stdout` within the 256 KiB and 1 MiB bounds; a log that
   cannot be fetched leaving the outcome unchanged; each reason of
   3.6; local failure skips the push; resume attaches without a second pull request or commit;
   a closed gate pull request is refused and never replaced by a second; a push interrupted
   after it landed is not recorded as refused, and resume attaches to it; a merge ref changed
   after one check passed refuses the later check; a repair round appends one commit; a head branch with the candidate
   tree but another base, a merge commit, or a foreign commit message is refused; the same Task
   ID from another Store is refused at the base; a successful job on a `push`-event run or on
   another workflow's run for the same head commit earns nothing; a run with two attempts is
   judged on the latest; a duplicated job name is ambiguous; a merge ref with another tree or
   other parents is refused; two remote-selected checks share one clock and the one whose jobs
   are incomplete at the limit is `NotRun` while the other passes; a failing push to a local
   bare repository whose diagnostic contains its URL records `<push-url>`; a mapping with user
   information in `push_url` is refused; a stored result that mixes the local and remote
   shapes, or whose status its evidence does not derive, is refused by the reader;
   cancellation during the wait ends the subprocesses.
8. One ADR (0139) recording the decision; the four choices made on 2026-10-04 (draft pull
   request over a bare gate branch; the kernel pushes under an operator mapping over a
   host-pushed commit; per-machine selection over policy-fixed; refusing `.github/` changes
   over flagging them) with the rejected options; and that it supersedes, for
   operator-authorized gate branches and the gate pull request only, the rule that publishing
   is a human action. The same exception is written where the rule lives: the closing paragraph
   of `docs/values.md` and the invariants of `AGENTS.md`, beside the unchanged delivery
   invariant. `CONTEXT.md` gains *Remote Check*, *Remote Check mapping* and *Gate pull
   request*; `docs/task-execution/remote-checks.md` is the walkthrough (declare, map, run, read
   the evidence, clean up, what a workflow must allow); `docs/non-goals.md` keeps the delivery
   non-goal and points at the ADR; `changelog.d/remote-checks.md` carries the release note.

Acceptance:

- With no mapping, a Task over a policy that declares `remote` produces the same check results
  and receipt as the same policy without the table, apart from the policy identity.
- With a mapping, a passing fixture records `Passed` with `observed` evidence naming the pull
  request, the base, head and merge commits, the run with its attempt, and each required job;
  the pushed head commit's tree and the merge commit's tree both equal the candidate manifest
  entry for entry.
- A fixture whose local `fmt` check fails pushes nothing and records the remote check `NotRun`
  with `remote_skipped_local_failed` and `refused` evidence.
- Running the same check Attempt twice against the same remote state creates no second pull
  request and no second commit; running it against a head branch this Task did not make
  refuses and pushes nothing.
- No test, fixture, recorded artifact, event or command output contains a credential, the push
  URL or a mapping path.
- A failing fixture's check result carries the failed jobs' log tails as its `stdout` artifact
  within the stated bounds, and `af task show` says an excerpt was kept.
- No `git push` invocation in the implementation carries `--force` or a `+` refspec, and no ref
  outside `refs/heads/af-gate/<task-id>/` is ever written.
- `docs/values.md` and `AGENTS.md` state the exception in the same change that introduces the
  behaviour.

### RC2 — Adoption and live proof (no package code)

By hand, after RC1 is delivered and released into a build this repository can run:

1. Add `[checks.kernel.remote]` to `.af/code-policy.toml` with the workflow path and job names
   read from a real pull-request run of this repository.
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

Campaign `remote-checks-design`, 2026-10-04: one reviewer (GPT-6 Sol, high), 162,258 tokens,
7 minutes, 9 Findings (7 blockers, 2 majors), no Demands. Eight are fixed and one is rejected
in this revision; per [ADR-0037](../adr/0037-default-campaigns-to-one-round-light-review.md) no
second Campaign follows.

| Finding | Disposition |
| --- | --- |
| Kernel publication conflicts with the human-publication rule | Fixed: §2.5 names the exception; deliverable 8 and the last acceptance line require `docs/values.md`, `AGENTS.md` and ADR-0139 to state it in the same change. |
| An existing head branch is trusted by tree equality alone | Fixed: 3.4 steps 3 and 5 give gate commits an owning Task identity and verify the whole chain down to this Task's base before attaching or appending. |
| Pull-request checks are polled on the wrong commit | Partly rejected, partly fixed. Rejected premise: a `pull_request` run does report under the pull request's head SHA (read on this repository's PR #155: run event `pull_request`, `head_sha` equal to the head commit, jobs listed under it). Fixed concern: 3.4 step 8 now reads the merge ref back and requires its parents and tree, so the tested tree is proven, not inferred. |
| A check name alone cannot identify the run | Fixed: 3.1 declares the workflow path; 3.4 step 7 admits only jobs of that workflow's `pull_request` run for this pull request and head commit, latest attempt, and makes duplicates `remote_check_ambiguous`. |
| The remote result does not fit the check contract | Fixed: 3.5 defines a second `CheckResult` shape and the reader's remote branch. |
| Raw Git diagnostics can expose the push URL | Fixed: 3.4 redaction rule, with a test in deliverable 7. |
| CI logs cannot satisfy the no-credential rule | Rejected by the owner on 2026-10-04, after a first revision had dropped the logs: they are needed to debug a remote failure. Bounded tails of failed jobs are kept (3.4 step 10); the exposure and why it is accepted are in 3.8 and go into ADR-0139. |
| Evidence has no shape for pre-push refusals | Fixed: three tagged states in 3.5. |
| The per-check wall limit is undefined for a shared wait | Fixed: one remote-phase clock in 3.3, with a two-check fixture. |

### Packages

**RC1 — implementation.** Task `remote-checks-rc1` (`kernel/implementation-reviewed`; plan
`sha256:0ffa4271…`), planned at commit 69ce704, 2026-10-04: 963,291 tokens, 7 Attempts, about
92 minutes. The gate passed (`kernel` 632.8 s cold, `markdownlint` 15.8 s). The Task ended
`changes_requested`: two reviewers (GPT-6 Sol, high) reported five Findings, three distinct, and
the evaluator failed on two points. The candidate (Snapshot `sha256:1018f4c5…`) was committed
unmodified as 14c15f9.

| Finding | Disposition (commit dd32598, by hand) |
| --- | --- |
| A merge-ref proof was reused for a later check (both reviewers, evaluator) | The proof is read again for every batch of checks being judged; a test changes the merge ref after the first check passed. |
| An interrupted push was recorded as `refused` (both reviewers) | The two branches are read back; unreadable ends the Attempt with a kernel error and no evidence. A test stalls the remote after it accepted the push. |
| A closed gate pull request was replaced by a second one (bugs reviewer) | Pull requests are looked up in every state; a closed one is `remote_pr_refused`. |
| The Task owner could be equal in two Stores (evaluator) | The opening writer carries 64 bits of operating-system randomness, journaled in the opening transition. |

The Task was planned before the owner's decision to keep job logs, so its implementer built the
variant without them. The log excerpt of 3.4 step 10, its result-shape rule, the `log_id` of
`af task show --json` and their tests were added by hand in the same commit.

The mapping, the evidence and the result contract are as designed, with one simplification
found while adding the logs: the excerpt is the result's `stdout` and the evidence has no field
for it, since no decision reads a log.

**RC1 — verification.** Task `remote-checks-rc1-verify-6` (`kernel/verification-reviewed`), on
commit f4b05be, 2026-10-05: **verified**. 319,025 tokens, 5 Attempts; gate passed (`kernel`
500.4 s cold, `markdownlint` 12.4 s); the evaluator passed every deliverable and acceptance
line. Five earlier attempts never reached a reviewer and cost about 8,000 tokens together: one
failed on a test of the hand fixes that replaced a fixture copied with the gate tree's read-only
mode (fixed in f4b05be), three on load-sensitive tests of other crates while other sessions
loaded the machine (load 20 to 119; lease expiry and capture retries), and one died when the
machine slept.

The two reviewers left three Findings on the verified tree, fixed by hand afterwards and not
re-verified, as §5 allows one re-verification:

| Finding | Disposition (after the verdict) |
| --- | --- |
| A gate-shaped commit naming another Snapshot could be adopted (correctness, blocker) | Every head commit must name a Snapshot of this Task's Store and carry its tree; only a tip naming the current candidate is attached. Two forged variants are tested. |
| The read-back after an interrupted push could never run (bugs, major) | The read-back is one recovery call with its own 20-second bound; a landed push is recorded `published`. The test now expects that record and a successful resume. |
| A listing cut at ten pages could still pass (correctness, major) | An unexhausted run or job listing is an error for that poll: nothing is judged from it. |

Cost of RC1: 963,291 tokens to implement, 327,000 to verify, 162,258 for the design review.
- RC2: not started.
