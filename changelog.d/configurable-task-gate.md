- Configure the local Task gate through `.config/task-gate.json` (`af.task-gate/1`) and
  `scripts/verify.sh --profile task`, with CLI overrides for class labels and `release-check`
  steps, and exclude `e2e` and `dev-release-tests` from local Task gates while keeping them in
  pull request CI, `make check-fast` and `make check`
  ([ADR-2026-11-27-configure-the-local-task-gate-profile](docs/adr/2026-11-27-configure-the-local-task-gate-profile.md)).
