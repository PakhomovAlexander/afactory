- Add explicit fixed-rate Ant intervals down to one second and foreground scheduler run/stop/status,
  preserving daily timezone schedules, bounded missed ticks and one active Run per Ant.
- Add generic named Markdown result documents, author-defined tables and prose, read-only watch,
  freshness metadata and atomic file output. Ant contracts use generation 6 and Scenario Stores 7;
  previous generations are refused without migration. See
  [the decision](docs/adr/2026-11-11-native-intervals-and-recorded-result-markdown.md).
- Remove caller-only `owner-impl` packages and provider bindings from shipped authority;
  retain the Task's separately captured execution authority and unchanged catalog audits.
