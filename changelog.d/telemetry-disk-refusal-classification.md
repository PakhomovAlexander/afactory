- Correct the telemetry runtime smoke test to require `incomplete` for a disk preflight refusal,
  matching native non-budget failure classification. Keep the exact disk refusal, inconclusive
  acceptance, single attempt, runtime evidence and cleanup checks. Offline regressions exercise
  the smoke assertion path; they do not establish a new contained runtime or durability result.
