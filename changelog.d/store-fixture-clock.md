- Make the Store wide-usage and Review provenance fixtures deterministic with scoped unit-test
  time, retaining their original reservation and verification limits. Add real Store dispatch
  boundary, durable accounting and clock-isolation regressions
  ([ADR-0220](../docs/adr/0220-control-store-fixture-time-with-a-scoped-thread-clock.md)).
  Qualify the concurrency fixture's `Barrier::wait` calls to avoid false child-reap matches
  in the unchanged source guard, preserving simultaneous clock synchronization.
