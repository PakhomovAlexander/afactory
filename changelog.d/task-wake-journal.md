- Repair duplicate Task wake boundaries from maintenance leases and allow checked histories longer
  than one scan page to advance. Add bounded internal Store registration, atomic checkpoint/outbox
  persistence and a collection barrier with historical inspection. Automatic detached wakes,
  authenticated host delivery and retention compaction remain unimplemented; see the
  [proposed decision](../docs/adr/2026-11-13-persist-checked-task-wake-pages.md).
