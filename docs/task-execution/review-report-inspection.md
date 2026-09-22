# Review accounting inspection

`af review report --format json` uses
[`af/review-report@3`](../../schemas/review-report-v3.json), or
[`af/review-report@4`](../../schemas/review-report-v4.json) when a native usage component
requires the wider representation. A Campaign whose first Task capture failed has no Task
accounting and keeps the `af/review-report@1` label. The `task_accounting` array reads each
distinct Task's validated common execution ledger, including failures before the first
canonical report or selected Reviewer output. Inspection opens the Store and CAS read-only and
does not create events, receipts or missing artifacts.

Each Task entry reports current cumulative `chargeable_tokens`, outstanding `reserved_tokens`,
and started Attempt counts. Token totals and per-Attempt cumulative charges are canonical
u128 decimal strings. Attempt counts and original
reservation caps remain canonical u64 decimal strings. Native usage components retain u64 bounds
in generation 3; generation 4 also permits exact u128 components accumulated across native turns.
Sequence numbers and wall-clock fields are JSON numbers.

Counts include failed, abandoned and fenced work, and exclude reservations released before
starting. The three categories add up to the common budget's started Attempt count. A Provider
admission shared by several binding slots appears once, with all its slots. Reviewer and Scatter
Worker Attempts are business work; gates and other operations have their own category.
Each Attempt retains its original plan, invocation and reservation identities. Classification
and Review Round attribution use that plan's captured graph and inputs, including after a later
plan replaces the active graph. Wall-clock sidecars contribute display details; their Task-local
Round and epoch fields do not replace captured Review Round authority.

Each Round row's `task_chargeable_tokens_at_report` and `task_accounting` preserve the
cumulative total and exact Task log prefix its RunReport@6 recorded. These are snapshots, not
Round costs. Reports of 100 and 150 tokens for one Task contribute one current Task total of
150, not 250. Late usage can raise that current total to 170 while both reports remain
unchanged. Inspection reads the ledger's effective charge, which can exceed the original
terminal receipt, and never subtracts snapshots to infer Round costs or changes an original
reservation cap.

Text and Markdown distinguish “Task cumulative charge at report” from current “Task accounting”.
The same historical snapshot label is used by Campaign listing.
