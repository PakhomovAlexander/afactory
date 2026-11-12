- Fix Ant rolling admission after native charge overruns: retained and retired charges now saturate
  the entire budget comparison, recording `skipped_budget` and advancing the durable cursor.
  Preserve exact cumulative accounting, unknown reservations and conservative expiry. See the
  [decision](../docs/adr/2026-11-11-saturate-complete-ant-rolling-admission.md).
