- Every pull request description states the lines the pull request adds per directory, in a
  `Lines added:` block that `python3 scripts/pr-lines.py` prints (`crates/12k docs/1.1k
  tests/5k`; a file under a `tests` directory counts as tests, a root file under its own name,
  figures rounded past 1000), and the `PR report` check recomputes the figures from the pull
  request's commits and refuses a description whose block is missing or stale
  ([ADR-0195](docs/adr/0195-state-the-lines-a-pull-request-adds-per-directory-and-check-them.md)).
