- Calibration cohorts can now be declared, run and judged. `cli.py cohort-declare` fixes a
  cohort's logical members, seeds, days and CPU/image groups before any member runs; GitHub's
  run IDs are recorded beside it as provenance, so its bytes never change. Preparation and
  slowdown members build both sides with the default-off `perf-calibration` feature, and only
  the candidate runs a deterministic CPU loop inside its timed operation. Shipped builds refuse
  the loop's control. `cli.py calibration-prepare` seals A0 and the loop counts before any
  trial. `cli.py readiness` recomputes readiness from each member's original retained run. It
  refuses reruns and replacements and requires all 20 A/A members within tolerance and 9 of 10
  power trials valid in every critical case of the same trial. No real cohort has run yet.
  Also fixed: an unreadable or inconsistent claim from the trusted release handoff now stops a
  release measurement instead of allowing a second one. The authority probe's timed region takes
  a fresh lease of its own after setup. The supervisor kills again a process that a single group kill
  missed while it was being forked (`af.perf-run/5`, `af.perf-cohort/2`,
  `af.perf-calibration-parameters/2`, `af.perf-readiness/2`;
  [ADR-0181](docs/adr/0181-declare-cohorts-before-they-run-inject-only-the-candidate-and-count-whole-power-trials.md)).
