Fix telemetry collection interruption boundaries: revoke HTTP exchanges before terminal
accounting, synchronize socket ownership with cancellation, and close history lock descriptors
even when the deadline interrupts open or flock. A blocked cancelled resolver remains explicitly
unconfirmed and cannot perform later network operations.

Reconcile diagnostic retained counts, sealed/open execution summaries and both reader inventories
during capture and report admission. Contradictions are refused; observational differences stay
partial. Add production-path interruption and hash-consistent report regressions without changing
schemas, budgets, calibration evidence or statistical policy.
