- Activation closeout of the performance laboratory. A new `Performance activation` workflow
  verifies a proposed activation with the base revision's own code and a read-only token
  scoped to that one step. When the manifest or a record it names changed, it runs `cli.py
  activate --verify-proposed`, which recomputes readiness and a bridge's evidence from the
  retained Actions evidence. An unchanged activation is verified locally, and `--fetch` is gone.
  Active plans carry their calibrated CPU/image groups. A run measured on any other host cannot
  satisfy the required check (`uncalibrated_cpu_group`). An equivalent-adapter bridge measures
  as active only with verified executions: a golden check of both product sources and a
  post-merge A/A, recorded by the new `cli.py equivalence-evidence`. Its protected diff now
  covers lock files, Cargo manifests, build scripts, the toolchain and Cargo configuration.
  Readiness binds each original's head commit and refuses another producer. It names
  history gaps from the listed numbers alone. Capped or inconsistent listings are refused.
  `cost-report` reads each run's cache from its build manifest, keeps failed attempts and needs
  five distinct completed comparisons per group. `shipping-report` refuses repeated runs and
  other policies, and keeps failed or missing samples visible. No real cohort, activation,
  bridge or demonstration has run (`af.perf-run/6`, `af.perf-readiness/5`,
  `af.perf-activation/3`, `af.perf-equivalence/1`, `af.perf-activation-verification/2`,
  `af.perf-cost/2`, `af.perf-shipping/2`;
  [ADR-0184](docs/adr/0184-verify-proposed-activations-in-trusted-ci-and-bind-bridges-groups-and-costs-to-retained-executions.md)).
