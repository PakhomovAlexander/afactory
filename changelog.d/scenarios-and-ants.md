- Add durable `af scenario` orchestration of existing Tasks, child Scenarios, signed decisions,
  bounded actions and waits, with pinned definitions, recovery, cancellation and safe owned-file
  cleanup. Publication requires separately scoped machine authority and an exact human signature
  ([ADR-0274](../docs/adr/0274-orchestrate-pinned-scenarios-and-admit-ant-occurrences.md)).
- Add `af ant` daily scheduling with explicit pinned timezones, unique occurrence receipts,
  overlap and missed-occurrence skips, pause/disable operations, and per-run and rolling budgets.
  Ticks run locally; no daemon, cron job, real instance or production grant is installed.
