- `scripts/test-test-time-report.py` no longer pins the live exclusive list of
  `.config/nextest.toml`: it reads the expected filter and its `test(/.../)` clauses from that
  file's raw text, independently of the TOML subset parser it checks, and keeps the exact-list
  assertions on the committed fixture `fixtures/test-time-report/nextest.toml`. Changing which
  tests run alone no longer breaks `make preflight-check`.
