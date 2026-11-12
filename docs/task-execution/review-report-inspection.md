# Review accounting inspection

`af review report TASK_ID --json` reads the same `af/task-report@3` document as
`af task report TASK_ID --json`. `af review inspect TASK_ID --json` reads
`af/task-inspection@20`: current and recorded plans, exact Attempt accounting, checked Task
history, selected Review Rounds, execution outputs and recovery evidence. There is one Task
Store and one cumulative allowance. A missing accounting observation remains absent or unknown;
report snapshots are never added together as independent spend.

`af review ledger TASK_ID --json` prints `af/review-ledger@3`: the complete selected canonical
Finding Set envelope, original Report envelopes keyed by exact ID, and an `inspection` view
with current Finding statuses, severity counts and reviewer coverage. Reports retain their
original producer and Snapshot; the inspection's reviewer result IDs describe current Round
coverage. Not-reproduced and dispute-only later Rounds never hide unresolved original Findings.
`af task findings` remains a current-Round report view. Its agent prompts target the selected
Round's Snapshot; original claim provenance is retained separately by the Ledger.
Missing siblings remain visible as recorded results with a null canonical Finding Set; they
cannot form a partial authoritative Ledger or satisfy closure. `--round N` selects a recorded
Round. Text includes Finding and Report identities; `--format sarif` uses the common SARIF
profile with complete canonical Findings, current statuses and original Report provenance in
each result's message.

`af review export TASK_ID PROPOSAL_ID` validates the sealed candidate and its original
Report/Finding linkage before printing patch bytes; `--finding FINDING_ID` selects a unique
applicable proposal. A changes-requested Review can still expose valid repairs. Automatic
Integration eligibility and passing whole-Task acceptance are separate from human-directed
export. A changed current head requires explicit `--allow-stale` authority. Export never promotes,
creates a branch, pushes or publishes.

The previous `af/review-report@5`, `af/review-ledger@1`, `af/review-show@1` and
`af/review-render@2` documents are retired. Their schemas and frontend readers are removed;
there is no migration or fallback reader. See [the facade](campaign-review.md) and
[the capability record](../design/current-contracts-and-task-review.md) for current paths, tests
and completed consolidation with its validation boundaries.
