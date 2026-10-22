- Every pull request description states the lines the pull request adds per directory, in a
  `Lines added` block that `python3 scripts/pr-lines.py` prints: a title with the total and a
  table with one row per directory — its figure, and its share as a bar and a percentage (a
  file under a `tests` directory counts as tests, a root file under its own name, figures
  rounded past 1000) — and the `PR report` check renders the block again from the pull
  request's commits and refuses a description whose block is missing or differs
  ([ADR-0195](docs/adr/0195-state-the-lines-a-pull-request-adds-per-directory-and-check-them.md)).
