Compose issue 834's scoped, bounded implementation repair/publication driver with PR1037's
native nightly release and daily Ant recovery. Preserve completed-child deadline handling,
fresh mutation guards, external nightly signing and exact native accounting. See the
[combined contract decision](../docs/adr/2026-11-17-compose-scoped-implementation-and-native-nightly-contracts.md).

Authority compatibility: Scenario/Bundle 9, Policy 8, Pin/Run/List 10, Store 15,
adapter request 7/response 3, Ant 10, Pin/Tick 11, State/List 12, inspection 7,
workspace state 5 and GitHub state 8 replace both parents' closed shapes. Finish or discard
prior-version state; no migration, aliases or historical evidence rewriting. Isolated recovery
checks make no live paid Scenario or physical VM reboot claim.
