- `scripts/test-quarantine.py confirm` runs the quarantined tests (or the `--only` ones) in N
  whole nextest runs (`--runs`, default 50) with the functional class running beside them as load
  (`--load functional`, or `none`), never stopping early or rerunning a failure, and writes one
  `af.test-quarantine-confirm/1` report classifying each as `flaky`, `always_fails` or `clean`; a
  run without a result writes no report. `scripts/test-quarantine.py apply REPORT --write
  --issues DIR` makes a flaky entry `state: flaky` for 14 days with a `flaky: <identity>` issue
  body, removes an always-failing entry with a P0 body, and removes a clean entry only when the
  report's platform is the evidence platform (`--evidence-platform`, default macOS); it never
  runs `gh`. `docs/test-quarantine-claw.md` gives the Claw runbook
  ([ADR-0268](docs/adr/0268-quarantine-flaky-tests-on-gate-evidence-and-confirm-them-on-claw.md)).
