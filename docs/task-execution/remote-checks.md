# Remote Checks

A Remote Check hands a declared code check to the repository's own GitHub Actions workflow
through a draft gate pull request, and binds the result to the exact Snapshot under check, so a
host too small to build the project can still hold a real Gate
([ADR-0136](../adr/0136-run-a-declared-check-through-a-gate-pull-request.md)). The design and its
fixed requirements are in [`docs/design/remote-checks.md`](../design/remote-checks.md).

## 1. Declare the check

Add a `remote` table to a check the project already declares. The `command` stays: it is the
check's local equivalent, and every machine without a mapping runs it exactly as before.

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

## 2. Map it on this machine

The mapping is an operator file outside every source tree:
`$XDG_CONFIG_HOME/af/remote-checks.toml` (`~/.config/af/remote-checks.toml`), or the absolute
path in `AF_TASK_REMOTE_CHECK_POLICY_FILE`.

```toml
version = 1

[[github_pr]]
repository_id = "5f1c…"   # git rev-list --max-parents=0 HEAD, sorted, joined by commas
github = "owner/name"     # the repository `gh` addresses
push_url = "git@github.com:owner/name.git"
checks = ["kernel"]
```

- `repository_id` is the identity Task source origins record, so one machine can map several
  repositories without ambiguity.
- `checks` names declared checks with a `remote` table; any other name is an error before the
  first check starts.
- `push_url` is any Git URL without user information. Authenticate through Git's credential
  helper or SSH, and log `gh` in once with `gh auth login`; `af` reads, stores and prints no
  token.

The mapping is the authorization for this machine to push the gate branches of that repository.
Delete it and every check runs locally again; there is no state to migrate.

## 3. Run

Start or resume a Task as usual. Within its check Attempt:

1. Local checks run first, in name order. If a required local check fails, each remote check is
   recorded `not_run` with `remote_skipped_local_failed` and nothing is pushed.
2. Otherwise one remote phase serves every remote check, on one clock that ends at the earlier of
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

## 4. Read the evidence

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
| `remote_candidate_changes_ci` | the candidate changes `.github/`; run this Task where the check is local |
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

## 5. Clean up

The branches and the pull request stay, so resume and repair rounds can use them. When the Task
no longer needs them, `af task show` prints the two commands:

```text
gh pr close <number> --repo owner/name
git push <push-url> --delete af-gate/<task-id>/base af-gate/<task-id>/head
```

## 6. What the workflow must allow

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
