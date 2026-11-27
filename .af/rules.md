# Standing rules for Task Workers

- Run only targeted tests for the crates, modules and scripts you touch (for example a scoped
  `cargo nextest run --workspace --locked --profile ci -E '<filter>'` or
  `python3 -I scripts/test-<name>.py`).
- Never run the whole workspace suite, the `e2e` class, `scripts/test-dev-release.py`, `make check`,
  `make check-fast`, `make check-classes` or `scripts/verify.sh` inside a Worker Attempt.
- After seal, the kernel runs the configured local Task gate (`scripts/verify.sh --profile task`,
  configured in `.config/task-gate.json`, which currently excludes `e2e` and `dev-release-tests`),
  and pull request CI runs the full fast validation (`functional`, `e2e`, `uix` and `base-check`
  including `dev-release-tests`).
