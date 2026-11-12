- `af task findings` now shows the exact recorded Demand Set, including required and advisory
  demands, their states, claims, methods and Evidence links, even when there are no findings.
  JSON uses `af/task-findings@3` and the enclosing Ledger uses `af/review-ledger@3`; previous
  versions fail closed. Empty findings and an evaluator pass do not discharge required demands.
- Demand text preserves printable Unicode, shell quotes and backslashes. Multiline methods
  appear on indented lines; terminal controls stay escaped so recorded text cannot inject rows.
