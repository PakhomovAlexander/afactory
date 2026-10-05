- Remote Checks: a declared code check may add a `[checks.<name>.remote]` table (`executor =
  "github-pr"`, `workflow`, `required` job names), and an operator's machine-local mapping
  (`$XDG_CONFIG_HOME/af/remote-checks.toml` or `AF_TASK_REMOTE_CHECK_POLICY_FILE`) selects it per
  repository. Local checks run first; the kernel then builds two `af-gate/<task-id>/` branches
  from the Task's Snapshots in a private repository, pushes them without force, opens one draft
  pull request between them, waits on the declared workflow's `pull_request` run for that head
  commit, and accepts the result only after reading `refs/pull/<n>/merge` back as the candidate
  tree. Every remote fact is one `af/RemoteCheckEvidence@1`; a remote `CheckResult@1` names it
  instead of a command; every refusal is `not_run` with a named reason; the last 256 KiB of each
  unsuccessful job's log (1 MiB per check) is kept as the result's `stdout`; no record holds the
  push URL or the mapping's path. `af task show` prints the pull request, run, jobs, kept log
  excerpt and cleanup commands, and `--json` carries the evidence under `remote_checks`. Without
  a mapping nothing changes. This is the one operator-authorized exception to "publishing is a
  human action"; delivery still never pushes
  ([ADR-0140](docs/adr/0140-run-a-declared-check-through-a-gate-pull-request.md)).
