# Experiment Task tutorial

Review these files, initialize this directory as a Git repository, and commit them.
Run `af catalog test --source . --json`, then `af task start --execute --file experiment.json --json`.

`.af/code-policy.toml` declares the measure `write` — `measure.py` writes `size.txt`'s number of
bytes into its private `$TMPDIR` and reports `bytes_written` — and the objective `smaller`.
The kernel measures the source as the baseline, the implementer's sealed candidate after its
checks pass, and compares the two; the evaluator runs only when the comparison passed.
Save the comparison with
`af task output experiment --port comparison --format markdown --output comparison.md --json`.
The Python command substitutes support exactly the supplied tutorial goal; no model runs.
