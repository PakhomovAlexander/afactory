- The staged issue-triage Task fits its budget of
  [`docs/design/issue-triage.md`](../docs/design/issue-triage.md) §6.1: each triager has
  40,000 tokens and 500,000 ms per Attempt, and `fixtures/issue-triage/example/task.json` plans
  with 150,000 tokens, 1,200,000 ms and 8 Attempts. `scripts/backlog-check-triage.py` now
  requires the verifier's own `Triage` block for every outcome and the seven identity flags
  (`--task-id`, `--document-id`, `--evaluation-id`, `--bundle-sha256`, `--main`,
  `--policy-version`, `--event-id`) for every merged record, exiting 2 without one; holds an
  issue before reading any input on a `class/security` or `triage/held` label from `--label` or
  `--owner-label`, printing the private `af/issue-triage-hold@1` with exit 3; reads the
  `Triage` block from the report's canonical bytes as well as its presentation; and counts a
  commit written as `commit <sha>`, as a code span or alone whatever its mix of digits and
  letters. `af schema show` prints `af/issue-triage@1`, `af/issue-triage-merged@1` and
  `af/issue-triage-hold@1`.
- Label holds return even with unreadable reports or malformed draft sections; non-held drafts
  whose `sections` is not an array are refused with exit 1. The triage script's tests run on
  Python 3.9 using only its standard library, including exact Worker allowance checks.
