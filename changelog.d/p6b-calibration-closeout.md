- Calibration readiness now binds each cohort member to its first dispatch rather than to
  whichever of its artifacts can still be read. The performance workflow names a calibration
  run for its cohort's digest and member as the run is created. `cli.py readiness` and
  `calibration-prepare` read the complete workflow-run history and that run's attempt-1 record,
  so an original that failed, expired or became unreadable keeps its place and no later
  dispatch replaces it. Each member's plan must also be exactly the plan its authority freezes
  for it again (`--repo`). Also fixed: the authority probe gives each setup write a lease of its
  own and times its region through a fresh, cold authority. Three real-time tests run alone,
  and `make lint` now builds and runs the calibration builds of the probe and `af`
  (`af.perf-cohort/3`, `af.perf-readiness/3`;
  [ADR-0182](docs/adr/0182-bind-each-calibration-member-to-its-first-dispatch-and-its-authoritys-plan.md)).
