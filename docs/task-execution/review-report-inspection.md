# Review accounting inspection

`af review report` keeps `af/review-report@1` and its numeric fields for historical Campaigns.
When a captured Review Task is present, JSON uses
[`af/review-report@2`](../../schemas/review-report-v2.json). The existing `spend` array still
describes legacy Review Attempts and Provider operations. The additional `task_accounting`
array reads each distinct Task's validated common execution ledger, including failures before
the first canonical report or selected Reviewer output. Inspection opens the Store and CAS
read-only and does not create events, receipts or missing artifacts.

Each Task entry reports current cumulative `chargeable_tokens`, outstanding `reserved_tokens`,
and started Attempt counts. Token totals are canonical u128 decimal strings; Attempt counts,
original reservation caps, per-Attempt charges and Provider usage components are canonical u64
decimal strings. Sequence numbers and wall-clock fields retain their ordinary numeric shape.

Counts include failed, abandoned and fenced work, and exclude reservations released before
starting. The three categories add up to the common budget's started Attempt count. A Provider
admission shared by several binding slots appears once, with all its slots. Reviewer and Scatter
Worker Attempts are business work; gates and other operations have their own category.
Each Attempt retains its original plan, invocation and reservation identities. Classification
and Review Round attribution use that plan's captured graph and inputs, including after a later
plan replaces the active graph. Wall-clock sidecars contribute display details; their Task-local
Round and epoch fields do not replace captured Review Round authority.

For RunReport@6 history rows, `task_chargeable_tokens_at_report` and `task_accounting` preserve
the recorded cumulative total and exact Task log prefix. These are snapshots, not Round costs.
Reports of 100 and 150 tokens for one Task contribute one current Task total of 150, not 250.
Late usage can raise that current total to 170 while both reports remain unchanged. Inspection
reads the ledger's effective charge, which can exceed the original terminal receipt, and never
subtracts snapshots to infer Round costs or changes an original reservation cap.

Text and Markdown distinguish “Task cumulative charge at report” from current “Task accounting”.
The same historical snapshot label is used by Campaign listing. Historical reports, legacy
numeric spend fields and their existing rendered labels remain unchanged.
