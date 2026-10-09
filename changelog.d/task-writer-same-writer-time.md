- A live Task writer is no longer fenced when its own heartbeat records a renewal between one of
  its operations reading the clock and that operation observing its lease or writing (#231). Each
  place that compares the current writer's clock with the last recorded time now judges the same
  writer and epoch at the later of the two: the heartbeat's lease observation
  (`EventStore::task_lease_state`), the projection's clock check when it applies a transition
  (`TaskProjection::apply`, which also records that time and ends a released lease there), and the
  review integration append (`append_integration_atomic`), which now stamps its transition before
  it is persisted, as every other Task append already did. The lease check of each Task write
  (`TaskProjection::check_lease`) and the append stamp already used this time. Another writer or
  epoch, a premature takeover, Task collection's own clock check, an expired lease and a finished
  Task are refused as before
  ([ADR-0128](docs/adr/0128-renew-a-live-task-writer-lease-through-its-own-connection.md)).
