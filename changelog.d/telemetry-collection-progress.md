Fix telemetry collection repeatedly selecting the same newest run prefix when completed
history exceeds the per-cycle run cap. List within the existing page bound, then rotate
bounded processing through a locked, atomically written attempted-run position. Old verified
results can reach history; missing, expired and refused results and higher attempts remain
eligible within lookback. A fixed upper run ID keeps new arrivals from extending a rotation.
Incomplete listings and capped or interrupted cycles report their
limits. The new `af.perf-telemetry-collection-position/1` scheduling record fails closed on
invalid state; the telemetry runbook documents operator reset and reachability limits.
Artifact admission, history bounds and the archive lane retain their contracts.
