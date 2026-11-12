- Reconcile waiter validation reuse, streaming canonicalization, indexed Gate history and all six
  full CLI waiter workloads with rc.3 native Task Review and current fixture contracts. Recheck
  Task and observed-stream watermarks after every display-artifact read so a concurrent completion
  refreshes status before timeout handling. Port evidence corruption/deletion regressions to native
  Review, retain isolated doctor hints and telemetry byte admission, and regenerate fixture evidence.
  [Decision](../docs/adr/2026-11-11-bracket-native-task-display-observations.md). Historical GNU CPU
  reductions are pre-reconciliation diagnostics; no rc.3 speedup is claimed.
