# Scope is evaluated per active Report claim

**Status:** accepted (2026-08-20)

Scope and severity are evaluated on each active Report claim, not copied onto a Finding and not
taken from the last Report admitted. A Finding blocks a diff Subject when any active Report is
in-scope at the configured severity gate; it is wholly out-of-scope only when every active Report
claim is out. The effective blocking severity is the highest severity among in-scope active
claims.

Report Scope is a deterministic projection of the immutable Report location and the exact Round
Subject referenced by the event log. It is rebuilt from those authorities rather than persisted
as another Report or event field. Evidence with no derivable exact Round Subject has no Report
Scope; readers present it as `unknown` and convergence treats it fail-closed.

## Considered options

- **Use the most recently admitted Report.** Rejected because a later Round's out-of-scope
  corroboration could mask an earlier active in-scope claim, or the reverse.
- **Stamp Scope on the Finding.** Rejected because locations and the cumulative Change Set can
  change across Rounds while Report evidence must remain immutable.
- **Persist a companion Scope artifact.** Rejected because Scope is fully determined by existing
  immutable authorities; another artifact could only duplicate them and introduce disagreement.
- **Evaluate active claims independently (chosen).** This matches the claim-preserving identity
  model and prevents an out-of-scope corroboration from masking an in-scope blocker.
