- Scenario terminal Task evidence retains exact native identity, checks, reviewed continuation and
  accounting separately from accepted receipts. Native and CI repairs use finite fresh Tasks and a
  shared scoped repair allowance; verifier/report Tasks consume total bounds separately. See the
  [repair decision](../docs/adr/2026-11-16-route-terminal-native-outcomes-to-bounded-repairs.md).
- HTTP publication connects before its final signed-expiry guard and disables implicit reconnects.
  Guard refusals after dispatch retain reconciliation-only recovery. See the
  [transport decision](../docs/adr/2026-11-16-guard-scenario-http-mutations-after-connection.md).
- Authority compatibility: changed Scenario, Run, Ant and adapter journal generations fail closed;
  old Stores and plans are not migrated. Signed publication remains required; scoped publication,
  continuous driving and full lifecycle proof remain pending Stages C and D of issue 834.
