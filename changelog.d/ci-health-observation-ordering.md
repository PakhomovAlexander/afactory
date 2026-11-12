- Preserve release failures across reruns and unfinished sibling checks. Green closure now uses
  exact attempt completion timestamps, never run-ID or API order; older or ambiguous successes
  cannot erase failure evidence or recurrence. Unfinished checks and runs become bounded
  omissions while independently completed failures and release alerts are retained. Version
  signature state to `af.ci-signature-state/2` with exact completion evidence, and reduce
  duplicated native refusal lifecycles in favor of table-driven validator coverage.
