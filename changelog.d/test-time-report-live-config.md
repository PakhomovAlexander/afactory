- `scripts/test-test-time-report.py` no longer pins the live exclusive list of
  `.config/nextest.toml`: it reads the expected filter and its `test(/.../)` clauses from that
  file's raw text, independently of the TOML subset parser it checks, and keeps the exact-list
  assertions on the committed fixture `fixtures/test-time-report/nextest.toml`. Changing which
  tests run alone no longer breaks `make preflight-check`. It still requires every live
  exclusive pattern to be a plain test-name pattern anchored with a trailing `$`, and fails
  naming the offending pattern otherwise, since an unanchored one would silently run more tests
  alone: a temporary local edit removing the `$` from one live clause made the script fail,
  while dropping or renaming anchored clauses left it passing.
