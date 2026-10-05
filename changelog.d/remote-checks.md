- Remote Checks are chosen by the pipeline, not by the machine. A Task pipeline's check node lists
  `checks` (run on this machine) and `remote_checks` (run through the check's declared remote
  executor); a project that wants both gates keeps two pipeline variants, and there is no
  per-machine switch. The machine-local mapping (`$XDG_CONFIG_HOME/af/remote-checks.toml` or
  `AF_TASK_REMOTE_CHECK_POLICY_FILE`) now names only where this machine may push gate branches
  for a repository: **its `checks` key is gone, and a file that still carries it is refused**. A
  pipeline with remote checks cannot be planned without a target, and its plan carries the effect
  `publish-gate` and the destination `github:<owner/name>`, which `af task plan` prints on
  `EFFECTS` and `SEND`, so confirming the plan is the consent; the target is read again before
  the check Attempt pushes. A pipeline without `remote_checks` plans and runs exactly as before
  ([ADR-0140](docs/adr/0140-run-a-declared-check-through-a-gate-pull-request.md), amended).
