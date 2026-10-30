- The `PR report` check no longer fails a pull request whose `Lines added` block is stale or
  absent: it renders the block from the pull request's commits and writes it into the
  description itself, changing only the text between `<!-- af-pr-lines:v1 -->` and
  `<!-- /af-pr-lines -->` (or inserting the block right after the `af task report` block), and
  passes. The job now holds `pull-requests: write`; it still checks out only the base branch and
  runs no code of the pull request. When the write is refused, the check fails as before and
  prints the block to paste, now for an absent or misshapen block too.
  `scripts/check-pr-report.py --refresh-into FILE` writes the refreshed description to FILE. The
  `af task report` block is still the author's, and its rules are unchanged
  ([ADR-0269](docs/adr/0269-let-ci-maintain-the-lines-added-block.md)).
