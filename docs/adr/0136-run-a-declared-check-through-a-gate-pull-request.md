# ADR-0136: Run a declared check through a gate pull request

Status: accepted, 2026-10-04. Supersedes, for operator-authorized gate branches and the gate
pull request only, the rule that publishing is a human action
([`docs/values.md`](../values.md), [`AGENTS.md`](../../AGENTS.md)). Delivery is unchanged
([ADR-0031](0031-deliver-verified-tasks-to-new-local-worktrees.md)).

Implements package RC1 of [`docs/design/remote-checks.md`](../design/remote-checks.md) under
that plan's §2 fixed requirements.

## Context

A Task check is a command this machine runs. On a host too small to build this repository
(2 vCPU, 3.8 GB RAM) a cold `make check` takes 43 to 60 minutes and was killed for memory three
times; on the repository's GitHub runner it takes about 12. Agents worked around it by hand:
they pushed the candidate, opened a draft pull request and watched CI go green. The kernel could
not use that result: nothing bound a CI conclusion to the checked Snapshot, so the Task stayed
unverified, correctly.

The CI run is the same check the policy already names, executed elsewhere. The kernel needs to
put exactly the checked Snapshot in front of that executor, wait for it inside the check
Attempt, and record evidence that the conclusion belongs to that Snapshot and no other tree.

## Options

The four choices below were made on 2026-10-04.

1. **What the executor runs on.**
   - *A draft pull request between two gate branches* — chosen. A `pull_request` run reports
     the pull request's head commit and GitHub publishes the test merge as
     `refs/pull/<n>/merge`, which the kernel can read back and compare with the tree it built.
   - *A bare gate branch, judged by its `push` run* — rejected. A push run proves nothing about
     a merge, many repositories run their full gate only on pull requests, and a push run of any
     branch can carry the same job names; nothing would tie a job to this Task's commit except a
     SHA the kernel cannot distinguish from a person's push.
2. **Who pushes.**
   - *The kernel, under an operator's mapping* — chosen. The kernel builds both commits from
     Snapshots in a private repository, so what is published is exactly what the Task already
     holds, and resume can verify what it finds.
   - *A host-pushed commit the kernel only watches* — rejected. The kernel would have to trust
     that a commit someone else pushed has the candidate's tree, which is the gap §1 of the
     design describes, and could not refuse a branch another Task made.
3. **Who selects a remote run.**
   - *Per machine, through an operator's mapping* — chosen. Authority to act on a remote belongs
     to a person at a machine, as Provider bindings do; the same commit runs locally on a large
     host and remotely on a small one.
   - *Fixed by committed policy* — rejected. A commit would then force a push and grant push
     authority to whoever runs it, and a machine without the remote could not run the check at
     all.
4. **A candidate that changes `.github/`.**
   - *Refused before anything is published* — chosen: `remote_candidate_changes_ci`. A Worker
     must never decide what the workflow that judges its work does.
   - *Published and flagged* — rejected. A flag is advice; a candidate that rewrites the workflow
     would already have run with the repository's secrets.

## Decision

### Declaration and selection

A check in `.af/code-policy.toml` may add `[checks.<name>.remote]` with `executor`
(`github-pr`, a closed set), `workflow` (a `.github/workflows/<file>.yml|.yaml` path) and
`required` (1 to 32 distinct job names, 1 to 128 characters each, exactly as the Actions jobs
API reports them). Its `command` stays mandatory and is the local equivalent. A policy without
the table captures byte-identically and keeps its policy identity; the schema stays
`af.code-task-policy/1`. The kernel never compares a workflow with a command.

The operator's mapping is `$XDG_CONFIG_HOME/af/remote-checks.toml`, or the absolute path in
`AF_TASK_REMOTE_CHECK_POLICY_FILE`. The coordinator resolves the path once when it builds the
code domain; the domain reads it bounded (64 KiB) and without following links, as it reads the
Rust toolchain mapping, and only when the captured policy declares a `remote` table at all.
`[[github_pr]]` entries name a `repository_id` (the Snapshot origin's sorted root commits), a
`github` `owner/name`, a `push_url` and the `checks` they select. An absent file is no mapping.
Anything else that is not a valid mapping — a check the policy does not declare or declares
without `remote`, a repeated repository — is an error before any check starts. A push URL may not
carry user information: no password anywhere, and no user name except the SSH login of an
`ssh://` or scp-like `login@host:path` URL, which names an account rather than a credential.

### Order and time

Within one check Attempt, checks declared with `remote` and listed for the Snapshot's
repository are remote-selected; the others are local and run first, in name order, exactly as
before. If any required local check did not pass, every remote-selected check is `NotRun` with
`remote_skipped_local_failed` and `refused` evidence, and nothing is pushed. Otherwise one remote
phase serves all of them, with one clock started before the `.github/` comparison: it ends at
the earlier of `check_process_wall_ms` on that clock and the Attempt's deadline. A check is
judged as soon as all its required jobs are complete; one still incomplete when the phase ends
is `NotRun` with the local check's deadline words. A remote-selected check materializes no
sandbox, prepares no toolchain and binds no warm directory; under `[warm]` it keeps its evidence
group with every declared kind skipped for the reason `remote`. A remote check costs zero tokens.

### Transport

The executor (`review_pipeline::task::remote_check::github_pr`) is the one seam the check
operator calls, with the candidate Snapshot, the phase deadline and the cancellation flag.

- The *source* is the candidate's root ancestor along `parent_snapshot_id`. A difference in any
  path, mode or content under `.github/` refuses the check before any subprocess runs.
- In a temporary bare repository the kernel owns, `git fast-import` writes both trees from the
  manifests (`100644`, `100755`, `120000`); every tree is read back with `git ls-tree -r` and
  compared path, mode and blob with its manifest. A mismatch is a kernel error.
- Gate commits have author and committer `af <af@localhost> 0 +0000`, no signature and a message
  the kernel parses back: role, Task ID, Task owner and Snapshot. The base has the source tree
  and no parent; the head is a child with the candidate tree.
- **The Task owner** is a digest of the transition that opened the Task's log in its Store
  (writer, epoch, the Store's clock and the first revision). It survives every resume, since a
  log only grows, and differs between Stores. The first revision's ID was considered and
  rejected: a revision is content-addressed, so two Stores that start the same Task file over
  the same source produce the same ID and would adopt each other's branches.
- Branches are `refs/heads/af-gate/<task-id>/base` and `.../head`; a Task ID that is not a single
  ref component is `remote_ref_invalid`. `git ls-remote` decides: a base with another commit is
  `remote_ref_conflict`; a present head is fetched and verified down to this Task's base — every
  commit with exactly one parent, the fixed identity and this Task's and owner's head message —
  before anything is pushed. A head whose tip has the candidate tree is attached; otherwise one
  child commit is pushed as a fast-forward. Every push is one `git push --atomic --porcelain
  --no-verify` of refspecs `<commit>:refs/heads/af-gate/<task-id>/{base,head}` built and checked
  by one function: never `--force`, never a `+` refspec, never another ref. A rejected push is
  `remote_push_refused`.
- The pull request from head to base is found, or opened as a draft titled `af gate: <task-id>`
  with a fixed body saying it is not for review or merge; a failure is `remote_pr_refused`.
- At once and then every 15 seconds the executor lists `pull_request` runs for the head commit,
  keeps those of the declared workflow path that list this pull request, and reads the jobs of
  each kept run's latest attempt only. No kept run 10 minutes after the push is
  `remote_check_missing`; two kept runs, or a job name twice, is `remote_check_ambiguous`; a
  completed run without a required job is `remote_check_missing`.
- Before any check is judged, the pull request and `refs/pull/<n>/merge` are read back: open,
  joining exactly the two gate branches of this repository, at the pushed head and base, and a
  merge commit whose parents are that base and head and whose tree is the candidate tree. A
  merge ref with another tree, or a pull request joining other branches, is refused at once; a
  stale one is re-read until the phase ends. Either way the failure is `remote_merge_mismatch`.
- All required jobs `success` is `Passed`; any `failure` is `Failed`; any other conclusion is
  `NotRun` with `remote_check_inconclusive`. A job that did not succeed keeps up to 32
  unsuccessful steps by name and conclusion and its URL. Job logs are never fetched.
- `git` and `gh` run with the operator's ambient authentication as coordinator subprocesses,
  through `review-process` supervision, each with its own wall inside the phase's remaining time
  and the shared kill path on cancellation; local plumbing runs without the operator's Git
  configuration. `GIT_TERMINAL_PROMPT=0` and `GH_PROMPT_DISABLED=1`. A missing `git` or `gh`, or a
  `gh` not logged in to github.com, is `remote_tool_unavailable`; `gh auth status` output is never
  kept. Every kept diagnostic is bounded to 2 KiB after the push URL, the mapping path and the
  private repository's path are replaced by `<push-url>`, `<mapping>` and `<gate-repository>`.

### Evidence and the result contract

One `af/RemoteCheckEvidence@1` artifact per remote-selected check
([schema](../../schemas/remote-check-evidence-v1.json)), bound to the candidate Snapshot, in
one of three states: `refused` (nothing written for the check), `published` (branches exist, no
job decided it) or `observed` (the jobs decided it, after the merge-ref proof). It is the only
remote fact replay reads.

`CheckResult@1` gains a second, distinct shape: `remote` names the evidence artifact, and there
is no `program`, `exit_code`, `stdout` or `stderr`, and `args` is empty. The schema states both
shapes and refuses a mixture. Where the reader compares a local result's command with the
captured definition, for a remote result it requires that the definition declares `remote`, the
evidence validates, its executor, workflow and required names equal the declaration, its
Snapshot is the receipt's, and the result's status is the one the evidence derives — a not-run
reason begins with the evidence's reason code. Anything else is refused like a result that
changed its captured definition. `af/TaskCheckReceipt@1` is unchanged.

`af task show` prints, per remote check, the executor, pull request, run and attempt, each
required job with its conclusion and duration, the unsuccessful steps, the refusal reason, and
the two commands that close the pull request and delete the branches; `--json` carries each
evidence document under `remote_checks` in `af/task-inspection@11`.

### The exception to "humans publish"

For a remote-selected check, and only under an operator's mapping, the kernel pushes exactly
the two `af-gate/<task-id>/` branches and opens one draft pull request between them. It never
force-pushes, pushes any other ref, merges, marks ready, closes, comments or deletes. Nothing
else is published: base and head are commits the kernel made from Snapshots the operator already
handed to the Task, so no local branch or history leaves the machine. This supersedes the
human-publication rule for those two branches and that pull request only. Delivery still never
commits, pushes, opens a pull request or invokes a remote.

## Consequences

- A small host can hold a real Gate: the check is the repository's own CI, bound to the exact
  Snapshot by a merge-ref proof, and recorded once.
- CI executes candidate code with whatever permissions and secrets the repository's workflows
  give a same-repository pull request. The mapping is the operator's statement that this is
  acceptable; the `.github/` refusal keeps a Worker from changing what those workflows are.
- The evidence proves which run and merge commit GitHub reported, not what the runner did; a
  compromised runner or account is outside the model, as a compromised local toolchain is.
- Gate branches and pull requests outlive their checks so that resume and repair rounds attach
  to them; RC1 never removes them, and `af task show` names the two commands that do.
- Out of scope, each a separate decision: Campaign `[gate]` checks, other executors, check runs
  of other GitHub apps, keeping job logs, collecting gate pull requests, using the gate pull
  request for delivery, remote `[measures]` and the review Workers' `execute-checks` shell.
- Rollback is deleting the mapping file: every check runs locally again, with no state to
  migrate.
