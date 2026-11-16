- Scenario HTTP publication connects before its final signed-expiry guard, disables implicit
  reconnection, and checks the remaining transport allowance before sending. Refused uncertain
  intents retain reconciliation-only recovery. This is a bounded transport correction, not the
  complete durable implementation Scenario; see the
  [decision](../docs/adr/2026-11-16-guard-scenario-http-mutations-after-connection.md).
