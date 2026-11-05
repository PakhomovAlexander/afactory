- Isolate core collection in the disposable Linux cancellation-test owner while preserving
  real SIGQUIT delivery, exact signal exit status and existing cleanup deadlines. Hosted
  timeout confirmation remains pending; see the
  [decision](../docs/adr/2026-11-05-isolate-signal-test-owner-core-collection.md).
