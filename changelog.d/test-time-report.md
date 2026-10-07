- `make test` now prints where test time goes after every nextest run, passing or failing:
  nextest's wall time and test count, summed test-seconds, achieved parallelism, the time spent
  in the exclusive block that `.config/nextest.toml` runs alone
  ([ADR-0124](docs/adr/0124-run-tests-in-parallel-processes-and-link-them-once.md)), failures,
  a duration histogram and the slowest tests. CI also writes it to the step summary. A Task
  gate (`scripts/verify.sh`) records every `make check` step's time beside its nextest reports
  and ends its output with the same summary. The JUnit an earlier run left is removed before
  nextest starts, and a JUnit whose testcases do not add up to its declared counts gets a
  warning instead of totals; the report never changes the test step's exit status. It runs
  under Python 3.9 and reads `.config/nextest.toml` without `tomllib`. `scripts/test-time-report.py compare BASE_JUNIT... -- HEAD_JUNIT...` compares runs
  by their per-side medians and lists per-test changes of at least 1 s and added or removed
  tests, as a Markdown table for a pull request description.
