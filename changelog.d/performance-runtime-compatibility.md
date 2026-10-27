- Provision bounded telemetry scratch of 16/20/24 GiB for PR, nightly/manual and release
  profiles, retaining AF's 10 GiB admission floor and the existing workload allowances plus
  filesystem headroom. Use deterministic serial corpus preparation under `--ipc none`.
  Add hosted regression coverage with actual release AF for admission and all three
  telemetry experiments; full hosted rollout and calibration remain separate obligations.
- Select the explicit first-run help oracle from `0.13.0-rc.1` (heading introduced by
  committed change `5ea4aedf`, contained in rc.1, rc.2 and stable), retaining the older
  `0.12.0-rc.3` surface and usage, required-command, version and exit assertions. This
  changes authority for a future cohort; frozen cohorts and historical evidence are unchanged.
