- Validate retained wake scope, checked boundaries, provenance and stored progress before both
  checkpoint restoration and cancellation-driven replay, and again inside page commit; reject
  live clock rollback through the final serialized contention check. Deterministic CLI regressions
  cover genuine cancellation, retained corruption, all three clock samples and later completion;
  native uncertainty and finite polling remain unchanged. See the
  [proposed decision](../docs/adr/2026-11-15-validate-retained-wakes-before-rebuild-and-fence-every-clock.md).
