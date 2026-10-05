- Remote Checks: a declared code check may add a `[checks.<name>.remote]` table (`executor =
  "github-pr"`, `workflow`, `required` job names), and a Task pipeline's check node chooses where
  each check runs: `checks` on this machine, `remote_checks` through the declared executor. A
  project that wants both gates keeps two pipeline variants; there is no per-machine switch. An
  operator's machine-local mapping (`$XDG_CONFIG_HOME/af/remote-checks.toml` or
  `AF_TASK_REMOTE_CHECK_POLICY_FILE`) names only where this machine may push gate branches for a
  repository: a pipeline with remote checks cannot be planned without a target, and its plan
  carries the effect `publish-gate` and the destination `github:<owner/name>`, which `af task
  plan` prints on `EFFECTS` and `SEND`, so confirming the plan is the consent; the target is read
  again before the check Attempt pushes. Local checks run first; the kernel then builds two
  `af-gate/<task-id>/` branches from the Task's Snapshots in a private repository, pushes them
  without force, opens one draft pull request between them, waits on the declared workflow's
  `pull_request` run for that head commit, and accepts the result only after reading
  `refs/pull/<n>/merge` back as the candidate tree. Every remote fact is one
  `af/RemoteCheckEvidence@1`; a remote `CheckResult@1` names it instead of a command; every
  refusal is `not_run` with a named reason; the last 256 KiB of each unsuccessful job's log
  (1 MiB per check) is kept as the result's `stdout`; no record holds the push URL or the
  mapping's path. `af task show` prints the pull request, run, jobs, kept log excerpt and cleanup
  commands, and `--json` carries the evidence under `remote_checks`. A pipeline without
  `remote_checks` plans and runs exactly as before. This is the one operator-authorized exception
  to "publishing is a human action"; delivery still never pushes
  ([ADR-0139](docs/adr/0139-run-a-declared-check-through-a-gate-pull-request.md)).
