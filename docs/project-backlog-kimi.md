# Private capture in a Kimi development session

Record a defect from the Project checkout as soon as you have an observed fact. You can
capture with no network access and while the daily Ant or publisher is paused.

```bash
report_id=worktree-a-boundary-001  # Choose a unique ID for this observation first.
af backlog record --report-id "$report_id" \
  --title "Boundary case returns false" \
  --observed "Running the current boundary fixture with its minimum value returned false." \
  --repro "Run the boundary fixture with the documented minimum." --json
```

Add `--evidence-file FILE` for relevant private UTF-8 evidence. Store the receipt privately.
Look for `data.local_capture = true`. If the receipt is lost, retry with the same Report ID,
inputs and Project Workspace at the same source revision. If Kimi is running inside a
materialized Worker tree, hand the fact, reproduction and evidence to the supervising developer
for capture from the originating checkout. Continue the current Task. The authority's daily
process owns classification, duplicate decisions, independent verification and ticket publication.

Verify local capture with `af backlog show "$report_id" --private --json`. The authority receipt
may arrive later through the signed transfer. Ordinary `af backlog list --json` shows verified
items only. Do not place Report contents or evidence in a PR or a public issue.

[Project backlog operator runbook](project-backlog.md)
