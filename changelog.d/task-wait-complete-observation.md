- Validate the complete Task wait status observation before timeout and settled returns,
  including newly built views and final Gate-history reads. Reconstruct changed observations
  before choosing an exit code, retaining the oldest display stream prefixes across reads.
  Deterministic source-built CLI regressions complete Tasks during initial/refreshed view
  construction and actual final history reads with an expired timeout, and cover settled
  returns. The 250 ms polling interval and historical performance claims remain unchanged.
  [Decision](../docs/adr/2026-11-11-bracket-native-task-display-observations.md).
