- Keep computed Scenario Run accounting exact in u128 when multiple native Task charges exceed
  u64 in aggregate. Reconciliation, validation and retention preserve every charge; rolling Ant
  admission records `skipped_budget` and advances its cursor. See the
  [clarification](../docs/adr/2026-11-12-keep-within-run-accounting-wide.md).
