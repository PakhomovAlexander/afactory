# ADR-2026-11-27-configure-the-local-task-gate-profile: Configure the local Task gate selection and exclude hosted-covered suites

**Status:** accepted (2026-11-27)

Supersedes the fixed `scripts/verify.sh --profile fast` local Task gate command in
[ADR-0251](0251-run-the-task-gate-on-the-fast-validation-profile.md); ADR-0251's shared
`test-build` receipt before class tests and separate build/test deadlines stand.

## Context

On a shared development Mac at `TEST_THREADS=4`, the local Task gate (`scripts/verify.sh --profile
fast`) reached 91 minutes 17 seconds and exhausted its 90-minute post-build test wall even when all
functional, e2e and uix tests passed. Two steps accounted for 78 minutes of that 91-minute run:
`dev-release-tests` (`scripts/test-dev-release.py`, 38 minutes 52 seconds in `release-check`) and
the `e2e` class (39 minutes 13 seconds, including 974 seconds in 17 exclusive tests). Everything
else — `test-build`, formatting, both clippy checks, lightweight release/preflight checks, the
`functional` class and the `uix` class — finished in roughly 13 minutes.

Pull request CI (`.github/workflows/ci.yml` and `.github/workflows/validate.yml`) already runs the
complete `release:fast` selection (`functional`, `e2e`, `uix` and `container-probes`) alongside the
complete `base-check` (including `dev-release-tests`) across parallel hosted runners on every pull
request. Hardcoding `--profile fast` in `.af/code-policy.toml` and unconditionally running every
`release-check` suite in `check-classes` prevented operators from tuning local Task-gate iteration
without altering PR CI or `make check`.

## Considered options

- **Raise `check_wall_ms` in `.af/code-policy.toml`.** Rejected: `.af/code-policy.toml` is already
  at the 90-minute ceiling (`5400000` ms) admitted without per-process caps, and longer local gates
  compound multi-round Task turnaround rather than solving duplicate local execution.
- **Change `release:fast` or `base-check` globally.** Rejected: pull request CI, `make check-fast`
  and release validation rely on `release:fast` (`functional`, `e2e`, `uix`) and `base-check`
  running `dev-release-tests`.
- **Add a versioned `.config/task-gate.json` contract (`af.task-gate/1`), `scripts/verify.sh
  --profile task` (with CLI overrides), and a dedicated `task-gate-check` target in `Makefile`.**
  Chosen: keeps configuration explicit, schema-validated and audited while leaving PR CI,
  `--profile fast`, `--profile full`, `make base-check` and `make check` immune to ambient
  exclusion variables.

## Decision

1. `.config/task-gate.json` (`schema = "af.task-gate/1"`, validated against
   `schemas/task-gate-v1.json` by `scripts/test-classes.py audit`) declares the repository's local
   Task gate configuration:
   - `labels`: non-empty unique selection labels from the supported class/profile label set
     (`test:*`, `no-*`, `release:fast`, `release:full`), resolved in the `gate` context through
     `scripts/test-classes.py`. Unrecognized, empty or contradictory labels fail closed.
   - `exclude_release_steps`: unique step labels from `Makefile`'s `release-check` target to omit
     during the local Task gate. Unknown step names and invalid or empty class selections fail
     closed.
2. `scripts/verify.sh --profile task` (and `scripts/verify.sh --gate-config FILE`, with optional
   `--label`, `--exclude-release-step` and `--include-release-step` overrides) resolves the
   configured plan via `scripts/test-classes.py task-gate`, writes the resolved step exclusions to
   its private run directory, clears inherited Make flag channels, records the shared `make
   test-build` receipt, and invokes `make task-gate-check` with `TEST_PLAN` and
   `AF_TASK_GATE_SKIP_STEPS_FILE`. Direct `make check`, `make base-check`, `make release-check`,
   `make check-fast`, `make check-full` and `make check-classes` hard-pin release-step exclusions
   to empty so ambient environment variables cannot weaken full, fast or CI checks. Skipped
   `release-check` steps print `NOT RUN` and emit no passing step receipt.
3. `.config/task-gate.json` initially configures `labels = ["release:fast", "no-e2e"]` (selecting
   `functional` and `uix`, marking `e2e` and `perf` as `NOT RUN`) and
   `exclude_release_steps = ["dev-release-tests"]`. `.af/code-policy.toml` invokes
   `scripts/verify.sh --profile task`, and `.af/rules.md` instructs Task Workers to run only
   targeted checks for touched paths during Worker Attempts.
4. `scripts/verify.sh --profile fast`, `scripts/verify.sh --profile full`, `make check-fast`,
   `make check`, main validation, release validation and pull request CI retain `e2e` and
   `dev-release-tests` unchanged.

## Consequences

- Local Task gates validate compilation, formatting, clippy, catalog/contract/pin preflight,
  `functional` and `uix` in roughly 10–15 minutes instead of exhausting the 90-minute wall.
- Operators can adjust local Task-gate class coverage and `release-check` exclusions declaratively
  in `.config/task-gate.json` or via `scripts/verify.sh` flags in `.af/code-policy.toml`.
- Regressions isolated to `e2e`, `container-probes` or `dev-release-tests` that are not exercised
  by targeted Worker tests are caught by pull request CI and `make check` before merge.
