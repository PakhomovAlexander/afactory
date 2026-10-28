- `af task findings --json` prints `af/task-findings@2` and `af review report --json` prints
  `af/review-report@5`: each finding carries an `agent_prompt` (its location on the reviewed
  Snapshot, the evidence, the requested change and how verification will judge the fix, and all
  of it as one prompt), and each round says what it covered per reviewer (`reviewed`, or
  `not_run` with why) and per path (`reviewed`, `partially_reviewed`, `not_reviewed`,
  `outside_round`). A reviewer that did not run now reads `not reviewed`, never `no findings`.
  "Verified fixed" appears only with the verification record and Snapshot that prove it.
  `--format sarif` on both commands prints the findings as one SARIF 2.1.0 log for
  code-scanning tools
  ([ADR-0248](docs/adr/0248-hand-review-findings-to-the-agent-that-will-fix-them.md)).
