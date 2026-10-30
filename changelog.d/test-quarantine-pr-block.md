- A pull request that adds entries to `.config/test-quarantine.json` now names them in its
  description: `python3 scripts/pr-quarantine.py` prints the block between
  `<!-- af-pr-quarantine:v1 -->` and `<!-- /af-pr-quarantine -->` (the title
  `**Quarantined tests added: N**` and each test's state, expiry and why), the template has a
  `Test quarantine` section for it, and the `PR report` check renders it again from the pull
  request's commits, refusing a missing or differing block, and a stale one when none is added
  ([ADR-0268](docs/adr/0268-quarantine-flaky-tests-on-gate-evidence-and-confirm-them-on-claw.md)).
