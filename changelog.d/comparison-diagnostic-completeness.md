- A native release comparison's trace lane is complete only when every case the aggregate
  verdict selects has a baseline and a candidate trace without omission whose region coverage is
  complete. A missing side, an omitted side, a `partial` or a setup-only `region_missing` trace
  leaves the lane failed with its reason, in `comparison-report` and `comparison-verify` alike;
  such an honest incomplete bundle still verifies as incomplete
  ([ADR-0176](docs/adr/0176-diagnose-cli-cases-and-contained-linux-runs-and-bound-every-step-and-seed-before-it-is-used.md)).
- Each diagnostic profile of a comparison must carry exactly the selection the bundled
  aggregate's verdict derives (mode, limit, parent verdict digest, cases and reasons) and a parent
  receipt naming that aggregate's result, plan and verdict. Valid children of a case the
  aggregate did not select, or of another aggregate, never complete a lane, however their
  results, states and the export manifest were resealed.
