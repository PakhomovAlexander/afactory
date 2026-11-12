- Bound native Ant history under the unchanged 16 MiB journal limit. Retain recent/latest and open
  work, expose compact provenance and exact accounting, expire eligible old output-index rows, and
  reserve native Run capacity before dispatch so admission pressure cannot consume settlement space.
  Completed bodies that cannot fit are explicitly unavailable with settled effect/charge authority
  retained. Store/state/inspection/list generations are 8/7/2/7 and refuse previous shapes without
  migration; Scenario definitions and Ant pins are unchanged. Preserve raw Markdown, daily/interval
  scheduling and the scheduler stop-race fix. See the
  [decision](../docs/adr/2026-11-11-bound-native-ant-retention-and-settlement-capacity.md).
