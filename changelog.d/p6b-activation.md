- Calibration readiness now detects a deleted original. A cohort records the performance
  workflow's run number when it is declared, and readiness requires every later run number in
  the history it reads, so a deleted first dispatch refuses rather than letting a later one
  stand in. Readiness also counts only successful original attempts. It recomputes A0, loop costs
  and counts from the A/A and preparation originals and refuses other sealed parameters. It
  invalidates a member with two artifacts of one expected name and requires its measuring jobs on
  fresh hosted runners. An impossible timestamp is invalid evidence, not a traceback. New
  `cli.py activate --verify` verifies an activation manifest: every active plan rehashes its
  cohort, parameter and readiness records and recomputes the decision, and `--fetch` recomputes
  readiness from the cohort's retained Actions evidence. A full plan outside the calibrated
  environment and recipe is unavailable. An equivalent-adapter admission may change only its
  named adapter files and registry entries, and owner approval, golden checks and a post-merge
  A/A stay pending. New `cli.py cost-report` and `shipping-report` compute the cost and
  shipping-budget demonstrations from retained runs. No real cohort, activation or demonstration
  has run yet (`af.perf-cohort/4`, `af.perf-readiness/4`, `af.perf-activation/2`,
  `af.perf-activation-verification/1`, `af.perf-cost/1`, `af.perf-shipping/1`;
  [ADR-0183](docs/adr/0183-detect-deleted-originals-recompute-parameters-and-verify-activations-from-retained-evidence.md)).
