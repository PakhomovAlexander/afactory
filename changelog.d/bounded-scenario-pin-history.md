- Validate Scenario template pins with at most two indexed history entries and bounded event
  bytes, preserving canonical pin checks without replaying corrupt streams under the Store writer
  lock. Unrelated Task heartbeats remain available during pinning. See the
  [Scenario authority decision](../docs/adr/2026-11-13-seal-local-worker-authority-in-scenario-templates.md).
