# Remote Checks

A Remote Check hands a declared code check to the repository's own GitHub Actions workflow
through a draft gate pull request, and binds the result to the exact Snapshot under check, so a
host too small to build the project can still hold a real Gate
([ADR-0140](../adr/0140-run-a-declared-check-through-a-gate-pull-request.md)). The design and its
fixed requirements are in [`docs/design/remote-checks.md`](../design/remote-checks.md).

Three places say three things. The code policy declares what a check's remote form *is*. A Task
pipeline's check node chooses *where* each check runs. The operator's machine-local mapping says
*where this machine may push*, and nothing else.

## 1. Declare the check

Add a `remote` table to a check the project already declares. The `command` stays: it is the
check's local equivalent, and every pipeline that lists the check in `checks` runs it exactly as
before.

```toml
[checks.kernel]
name = "kernel"
required = true

[checks.kernel.command]
program = "bash"
args = [{ value = "scripts/verify.sh", provenance = "literal" }]

[checks.kernel.remote]
executor = "github-pr"
workflow = ".github/workflows/ci.yml"
required = ["validation / lint", "validation / check (ubuntu-latest)"]
```

- `executor` is `github-pr`; any other name is refused when the policy is captured.
- `workflow` is the repository path of the workflow whose `pull_request` run earns the result.
  Jobs of any other workflow never count, whatever their names.
- `required` lists 1 to 32 distinct job names exactly as the Actions jobs API reports them for
  that run. A job of a called workflow reads `caller / job`, as in `validation / lint`. Read the
  names from a real pull-request run: `gh run view <run-id> --json jobs --jq '.jobs[].name'`.

Declaring both forms is the project's statement that they verify the same thing. A policy
without the table is captured byte for byte as before.

## 2. Choose it in a pipeline

The check node of a Task pipeline lists where each of its checks runs:

```toml
[nodes.operator]
op = "check"
checks = ["markdownlint"]        # run on this machine, first
remote_checks = ["kernel"]       # run by the check's declared remote executor
```

- `remote_checks` is optional. A node without it is byte for byte the node it was, plans to the
  same plan, and never reads the mapping.
- A check appears in one list only, and a node names at least one check in all; `checks` may be
  empty when `remote_checks` is not.
- Every name in `remote_checks` must be a check the captured code policy declares with a
  `remote` table. Anything else is refused when the plan is captured, naming the pipeline, the
  node and the check. The same holds in a child pipeline a parent calls.

There is no per-machine switch. A project that wants a local gate and a remote one keeps two
pipelines that differ only in these two lists — this repository stages
`kernel/gate-bench-remote`, `kernel/implementation-reviewed-remote` and
`kernel/verification-reviewed-remote` under
[`fixtures/remote-checks/packages/`](../../fixtures/remote-checks/packages/README.md) — and a
Task file names the one it wants.

## 3. Give this machine a push target

The mapping is an operator file outside every source tree:
`$XDG_CONFIG_HOME/af/remote-checks.toml` (`~/.config/af/remote-checks.toml`), or the absolute
path in `AF_TASK_REMOTE_CHECK_POLICY_FILE`.

```toml
version = 1

[[github_pr]]
repository_id = "5f1c…"   # git rev-list --max-parents=0 HEAD, sorted, joined by commas
github = "owner/name"     # the repository `gh` addresses
push_url = "git@github.com:owner/name.git"
```

- `repository_id` is the identity Task source origins record, so one machine can map several
  repositories without ambiguity.
- `push_url` is any Git URL without user information. Authenticate through Git's credential
  helper or SSH, and log `gh` in once with `gh auth login`; `af` reads, stores and prints no
  token.
- The file selects no checks. One that still carries the `checks` key of the first release is
  refused, with a message that the pipeline's check node chooses now.

The mapping is the authorization for this machine to push the gate branches of that repository.
It is read when a plan is captured: a pipeline with remote checks and no target for the source
Snapshot's repository is refused there, before any Attempt, naming the mapping and the
repository identity. Otherwise the plan's authority gains the effect `publish-gate` and the data
destination `github:<owner/name>`:

```text
EFFECTS execute-checks, publish-gate, read-source, write-source
SEND  github:owner/name
```

`af task plan` prints them, `af task plan --json` and `af task explain` carry them, and
confirming the plan confirms them. The push URL stays on the machine and is read again when the
check Attempt starts: a mapping that then names no target for the repository, or another
`github` than the plan recorded, ends the Attempt with an error before any check runs, and
nothing is pushed.

## 4. Run

Start or resume a Task as usual. Within its check Attempt:

1. The node's `checks` run first, in name order. If a required local check fails, each remote check is
   recorded `not_run` with `remote_skipped_local_failed` and nothing is pushed.
2. Otherwise one remote phase serves every check of `remote_checks`, on one clock that ends at the earlier of
   `check_process_wall_ms` and the check Attempt's deadline.
3. The kernel builds two commits from the Task's Snapshots in a private repository — `base`
   from the Task's captured source, `head` from the candidate — and pushes them as
   `af-gate/<task-id>/base` and `af-gate/<task-id>/head`, then opens one draft pull request
   from head to base (`af gate: <task-id>`). It never force-pushes and writes no other ref.
4. It waits on the declared workflow's `pull_request` run for that pull request and head commit,
   reading the latest attempt's jobs every 15 seconds, and reads the pull request and
   `refs/pull/<n>/merge` back before judging: the merge commit's parents must be the gate base
   and head, and its tree the candidate's.

Resuming the Task, or running the check again over the same Snapshot, attaches to the same
branches and pull request without a new commit. A repair round appends one commit to the head
branch. A branch this Task did not make — another Task's, or this Task ID's in another Store —
is refused with `remote_ref_conflict`, and nothing is pushed.

## 5. Read the evidence

`af task show <task-id>` prints, per remote check:

```text
remote check kernel: failed by github-pr on owner/name, pull request https://github.com/owner/name/pull/12, run 77 attempt 2
  job validation / lint: success in 70 s
  job validation / check (ubuntu-latest): failure in 750 s; unsuccessful steps: Run make check (failure); log at https://github.com/owner/name/actions/runs/77/job/1002
  log excerpt of the unsuccessful jobs: CAS object sha256:9f2c…
```

and a `reason:` line for every check that did not pass or fail. `af task show --json` carries
each `af/RemoteCheckEvidence@1` document under `remote_checks`
([schema](../../schemas/remote-check-evidence-v1.json)), with `log_id` when a log excerpt was
kept.

For every required job that did not succeed, `af` keeps the last 256 KiB of its log (1 MiB per
check) as the check result's `stdout`, each tail under a `==> job …` header. Read it from the
Task Store's CAS, where an object `sha256:<aa><rest>` is the file
`<state>/cas/objects/<aa>/<rest>`; the full log stays at the recorded job URL. The excerpt is
GitHub's text: GitHub masks the repository's registered secrets, and anything a workflow prints
in another form is stored as printed.

| Reason | What to do |
| --- | --- |
| `remote_skipped_local_failed` | fix the local failure |
| `remote_candidate_changes_ci` | the candidate changes `.github/`; run this Task with a pipeline that lists the check in `checks` |
| `remote_tool_unavailable` | install `git` or `gh`, or run `gh auth login` |
| `remote_ref_invalid` | use a ref-safe Task ID |
| `remote_ref_conflict` | another Task owns the branches; delete them or rename the Task |
| `remote_push_refused` | read the redacted diagnostic (a ruleset or a permission) |
| `remote_pr_refused` | read the redacted diagnostic |
| `remote_check_missing` | correct the job name, the workflow path or its trigger |
| `remote_check_ambiguous` | make job names unique, or cancel the stray run |
| `remote_merge_mismatch` | someone changed the gate pull request; restore or delete it |
| `remote_check_inconclusive` | rerun the job on GitHub, then resume the Task |

A remote check still incomplete when the phase ends is `not_run` with the deadline reason a
local check uses; resume the Task to attach again. A kept diagnostic names the push URL, the
mapping and the private repository only as `<push-url>`, `<mapping>` and `<gate-repository>`.

## 6. Clean up

The branches and the pull request stay while the Task runs, so resume and repair rounds can use
them. When the Task finishes, af closes the draft gate pull request and deletes both
`af-gate/<task-id>/` branches from the mapping's push target, with the same `gh` and `git`
([ADR-0144](../adr/0144-hold-afs-disk-use-to-a-machine-budget.md)), and only what still equals
the Task's recorded evidence. It closes a pull request only while it is open, its base repository
is the recorded one, its head and base are exactly the Task's two branches and its head commit is
the recorded head commit. It deletes a branch only while the push target shows exactly the
recorded commit for it, in one atomic push. A branch or pull request that differs is left in
place and named in the reason of a failed cleanup. It records the outcome as a `gate_cleanup` in
the Task's log, which `af task show` prints (`gate cleanup: done; …`) and `--json` carries under
`gate_cleanups`. A failed cleanup never changes the Task's result; the next sweep (after a run,
or `af storage prune --apply`) tries again while the mapping still names the repository, and
neither a sweep nor `af task gc --apply` collects the Task until a cleanup is done. Until then
`af task show` prints the two commands:

```text
gh pr close <number> --repo owner/name
git push <push-url> --delete af-gate/<task-id>/base af-gate/<task-id>/head
```

`[storage] keep_gate_pull_requests = true` in your machine configuration keeps them open.

## 7. What the workflow must allow

- It runs on `pull_request` for draft pull requests whose base is an `af-gate/**` branch. A
  workflow filtered to `main`, or one that skips drafts, never starts, and the check is
  `remote_check_missing` ten minutes after the push.
- Its jobs keep stable, unique names; the evidence matches names exactly.
- Point a Remote Check only at workflows you would run for a collaborator's branch: CI executes
  the candidate with whatever permissions and secrets a same-repository pull request gets. The
  kernel refuses any candidate that changes `.github/`, so a Worker cannot change what the
  workflow does.
- A ruleset that forbids pushing `af-gate/**` branches makes every remote check
  `remote_push_refused`.
