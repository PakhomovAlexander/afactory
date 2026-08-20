# Scope is evaluated per active Report claim

**Status:** accepted (2026-08-20)

Scope and severity are evaluated on each active Report claim, not copied onto a Finding and not
taken from the last Report admitted. A Finding blocks a diff Subject when any active Report is
in-scope at the configured severity gate; it is wholly out-of-scope only when every active Report
claim is out. The effective blocking severity is the highest severity among in-scope active
claims.

## Considered options

- **Use the most recently admitted Report.** Rejected because canonical reviewer order would
  decide convergence when two reviewers attach mixed-scope claims in one Round.
- **Stamp Scope on the Finding.** Rejected because locations and the cumulative Change Set can
  change across Rounds while Report evidence must remain immutable.
- **Evaluate active claims independently (chosen).** This matches the claim-preserving identity
  model and prevents an out-of-scope corroboration from masking an in-scope blocker.
