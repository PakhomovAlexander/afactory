# Report Task tutorial

Review these files, initialize this directory as a Git repository, and commit them.
Run `af catalog test --source . --json`, then `af task start --execute --file report.json --json`.

The author reads this committed tree in a clone that seals nothing back and writes a report
citing `README.md`, its last line and `report.json`. The kernel renders it, resolves every
citation against the exact source Snapshot, and dispatches the independent verifier only when
those checks pass; the verifier reads the same Snapshot. A report Task is never delivered.
Save the report with
`af task output starter-report --port report --format markdown --output report.md --json`.
The Python command substitutes support exactly the supplied tutorial goal; no model runs.
