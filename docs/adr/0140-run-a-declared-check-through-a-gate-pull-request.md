# ADR-0140: Run a declared check through a gate pull request

Status: accepted, 2026-10-04; amended 2026-10-05 ([the pipeline chooses](#amendment-2026-10-05-the-pipeline-chooses)),
which replaces the per-machine selection of option 3 and of *Declaration and selection*; amended
on 2026-10-07 by [ADR-0144](0144-hold-afs-disk-use-to-a-machine-budget.md): when the Task
finishes, the kernel closes its draft gate pull request and deletes its two `af-gate/` branches.
Supersedes, for operator-authorized gate branches and the gate pull request only, the rule that
publishing is a human action ([`docs/values.md`](../values.md), [`AGENTS.md`](../../AGENTS.md)).
Delivery is unchanged ([ADR-0031](0031-deliver-verified-tasks-to-new-local-worktrees.md)).

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
3. **Who selects a remote run.** Superseded on 2026-10-05: the pipeline chooses (see the
   amendment).
   - *Per machine, through an operator's mapping* — chosen in RC1. Authority to act on a remote belongs
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

*The selection paragraphs below describe RC1; the amendment of 2026-10-05 replaces them. The
mapping no longer selects checks: a pipeline's check node does.*

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
  log only grows, and differs between Stores: the opening writer is `cli-<pid>-<nonce>`, whose
  nonce is 64 bits from the operating system's random source, journaled in that transition and
  never drawn again. The PID and the clock alone were not enough — two Stores could open the
  same revision with the same PID in the same millisecond. The first revision's ID was
  considered and rejected: a revision is content-addressed, so two Stores that start the same
  Task file over the same source produce the same ID and would adopt each other's branches.
- Branches are `refs/heads/af-gate/<task-id>/base` and `.../head`; a Task ID that is not a single
  ref component is `remote_ref_invalid`. `git ls-remote` decides: a base with another commit is
  `remote_ref_conflict`; a present head is fetched and verified down to this Task's base — every
  commit with exactly one parent, the fixed identity and this Task's and owner's head message —
  before anything is pushed. A gate-shaped message is not lineage, so every head commit must
  also name a Snapshot this Task's Store holds and carry exactly that Snapshot's tree; a commit
  someone appended under this Task's name is `remote_ref_conflict`. A tip that names the current
  candidate Snapshot and has its tree is attached; otherwise one child commit is pushed as a
  fast-forward. Every push is one `git push --atomic --porcelain
  --no-verify` of refspecs `<commit>:refs/heads/af-gate/<task-id>/{base,head}` built and checked
  by one function: never `--force`, never a `+` refspec, never another ref. A rejected push is
  `remote_push_refused`. A push stopped by the deadline or by cancellation proves nothing either
  way, because the remote may have accepted the atomic update first. The two branches are read
  back through one recovery call with its own 20-second bound, which runs although the phase's
  deadline has passed or the Attempt is cancelled; it reads and never judges. Both as pushed is
  a push that landed: the phase goes on when it has time, and otherwise the check is recorded
  `published` with the deadline or cancellation reason, for resume to attach. Both as found is
  a refusal. When they cannot be read back the Attempt ends with a kernel error and no
  evidence, and resume reconciles what it finds.
- The pull request from head to base is looked up in every state. An open one is attached; when
  none exists one is opened as a draft titled `af gate: <task-id>` with a fixed body saying it is
  not for review or merge. A closed one is never replaced by a second: the check is
  `remote_pr_refused`, naming the pull request to reopen. Any other failure is also
  `remote_pr_refused`.
- At once and then every 15 seconds the executor lists `pull_request` runs for the head commit,
  keeps those of the declared workflow path that list this pull request, and reads the jobs of
  each kept run's latest attempt only. No kept run 10 minutes after the push is
  `remote_check_missing`; two kept runs, or a job name twice, is `remote_check_ambiguous`; a
  completed run without a required job is `remote_check_missing`. A run or job listing that ten
  pages of 100 do not exhaust proves nothing unique: nothing is judged from it, and the check
  ends at the deadline with that diagnostic.
- Before any check is judged, the pull request and `refs/pull/<n>/merge` are read back: open,
  joining exactly the two gate branches of this repository, at the pushed head and base, and a
  merge commit whose parents are that base and head and whose tree is the candidate tree. The
  proof belongs to the observation it was read with: every batch of checks about to be judged
  reads it again, so a pull request changed after one check passed cannot carry a later one. A
  merge ref with another tree, or a pull request joining other branches, is refused at once; a
  stale one is re-read until the phase ends. Either way the failure is `remote_merge_mismatch`.
- All required jobs `success` is `Passed`; any `failure` is `Failed`; any other conclusion is
  `NotRun` with `remote_check_inconclusive`. A job that did not succeed keeps up to 32
  unsuccessful steps by name and conclusion and its URL.
- For each required job that did not succeed the executor fetches the job's log and keeps its
  last 256 KiB, at most 1 MiB across the check, each tail under a header naming its job and cut
  at a line. The excerpt is the check result's `stdout`, where a local failure's output is; it
  is not evidence, and no decision reads it. A log that cannot be fetched is left out and
  changes no outcome.
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
is no `program`, `exit_code` or `stderr`, and `args` is empty; its `stdout`, present only when
the check did not pass, is the log excerpt. The schema states both shapes and refuses a
mixture. Where the reader compares a local result's command with the
captured definition, for a remote result it requires that the definition declares `remote`, the
evidence validates, its executor, workflow and required names equal the declaration, its
Snapshot is the receipt's, the result's status is the one the evidence derives — a not-run
reason begins with the evidence's reason code — and a log excerpt accompanies only evidence that
observed jobs. Anything else is refused like a result that
changed its captured definition. `af/TaskCheckReceipt@1` is unchanged.

`af task show` prints, per remote check, the executor, pull request, run and attempt, each
required job with its conclusion and duration, the unsuccessful steps, the CAS object of the
kept log excerpt, the refusal reason, and the two commands that close the pull request and
delete the branches; `--json` carries each evidence document, and `log_id` when an excerpt was
kept, under `remote_checks` in `af/task-inspection@11`.

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
- A kept log excerpt is GitHub's text, with the push URL and machine paths replaced like every
  kept diagnostic. GitHub masks the repository's registered secrets in job logs; a workflow that
  prints one in another form puts it in the Store, as it already puts it in the pull request's
  log, which every collaborator can read. The design review asked for logs to be left out for
  this reason; the owner decided on 2026-10-04 that a remote failure must be debuggable from
  the Store and that withholding the log was more protection than this boundary warrants.
- Gate branches and pull requests outlive their checks so that resume and repair rounds attach
  to them; RC1 never removes them, and `af task show` names the two commands that do.
- Out of scope, each a separate decision: Campaign `[gate]` checks, other executors, check runs
  of other GitHub apps, collecting gate pull requests, using the gate pull
  request for delivery, remote `[measures]` and the review Workers' `execute-checks` shell.
- Rollback is planning the Task with the pipeline variant whose check node lists the check in
  `checks`; with the amendment, deleting the mapping no longer turns a remote pipeline local, it
  makes it unplannable on that machine.

## Amendment, 2026-10-05: the pipeline chooses

Implements package RC3 of [`docs/design/remote-checks.md`](../design/remote-checks.md), under
§2 item 2 as amended that day.

### Why

The RC2 live proof planned a Task whose plan preview printed `SEND  none`; the run then pushed
the Task's source to GitHub. Where a check ran was decided by a machine file that neither the
pipeline nor the plan showed, so the one confirmation a developer gives — the plan — did not
cover the one thing the kernel publishes. And asked how to make a gate local or remote, the only
answer was "edit a file on each machine": the pipeline, where a developer reads what a gate
does, said nothing. The owner decided that the pipeline's check node chooses and that there is
no per-machine override.

### Options

- *The check node lists its remote checks* — chosen. What a gate does is read where it is
  defined, and two pipeline variants serve a project that wants both a local and a remote gate;
  a Task file names one.
- *The per-machine mapping RC1 shipped* — rejected. The plan cannot show a choice it does not
  make, so confirming a plan never consented to the push.
- *A pipeline default with a bindings override* — rejected. An override would put the choice
  back on the machine, outside the plan, and every reader would have to consult both.

### Decision

- **The check node.** The Task pipeline's `check` operator gains an optional, non-empty
  `remote_checks` set beside `checks`. A node without it serializes and plans exactly as before.
  A name in both lists, or a node naming no check, is refused when the pipeline's package is
  captured; a `remote_checks` name the captured code policy does not declare with a `remote`
  table is refused when the plan is compiled. Every refusal names the pipeline, the node and the
  check, in a root pipeline or a child a parent calls. The code policy installs each check's
  remote form as the operator signature `operator/check/remote/<name>` only when the check
  declares `remote`; that signature carries the effect `publish-gate`, so a Planner, offered
  only operators the Task's authority already permits, is never offered one. A generated
  proposal that lists `remote_checks` anyway is refused by name when its structure is checked:
  only an installed pipeline chooses a remote gate.
- **The mapping names push targets only.** `[[github_pr]]` entries carry `repository_id`,
  `github` and `push_url`. A `push_url` that is a github.com URL must name the repository
  `github` names, since the plan shows `github` as the destination; a URL on another host, or
  a local path, cannot be compared and is accepted as the operator wrote it. A file that still carries `checks` is refused with a message naming
  the pipeline's check node as the place that chooses. A pipeline without remote checks never
  reads the mapping.
- **The plan says so.** When a candidate pipeline's compiled graph has remote checks, the
  coordinator reads the mapping for the source Snapshot's repository before the revision has an
  identity. Without a target the pipeline is unavailable — planning refuses before any Attempt,
  naming the mapping's knob and the repository identity. With one, the revision's authority, and
  so the plan's, gains the effect `publish-gate` and the data destination `github:<owner/name>`:
  `af task plan` prints them on `EFFECTS` and `SEND`, and `--json` and `af task explain` carry
  them. A data destination is a policy name or such a `github:` repository. The catalog compiler
  requires the effect and exactly one `github:` destination when the graph has remote checks,
  and neither when it has none, so admission's recompilation refuses any other pairing.
- **The run reads the target again.** Before any check of a node with remote checks starts, the
  check operator reads the plan's recorded destination and the mapping. No target for the
  repository, or another `github` than the plan recorded, ends the Attempt with an error naming
  the knob and the repository; nothing is pushed. The push URL never enters the plan. The
  reader holds evidence to the same destination: a remote result whose evidence names another
  repository than its plan recorded is refused at admission and on replay.
- **Only local checks need their tools here.** Planning requires the executable of a required
  check's command only when some node lists that check in `checks`. A check that runs remotely
  everywhere can name a tool this machine lacks, which is what a remote gate is for.
- **Unchanged.** `checks` run first, then `remote_checks` through the executor, with RC1's
  order, clock, transport, evidence, result contract, refusal reasons and receipt; the receipt
  names every check of both lists, and output admission refuses a receipt whose check ran
  somewhere its node did not say. A node whose checks all run remotely records no local runtime
  evidence group without `[warm]`, since it ran nothing on the machine.

### Consequences

- A remote pipeline is a reviewed, committed choice, and its plan shows what it publishes; the
  operator's mapping remains the authority to publish, and one machine without it cannot plan
  the pipeline at all.
- This repository's remote twins are staged under
  [`fixtures/remote-checks/packages/`](../../fixtures/remote-checks/packages/README.md) for a
  person to install, since a Worker may not write `.af/`.
