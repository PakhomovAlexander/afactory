- Bare `af` opens its browser before any Task or Campaign Store is read: the Tasks, Campaigns and
  Workers panes read their Stores on a thread behind a spinner on their folder, Store by Store,
  so a machine whose registry names dozens of Stores no longer sits on the splash for as long as
  every event log takes to replay (40 s measured; issue #402,
  [ADR-0256](docs/adr/0256-open-the-browser-before-any-store-is-read.md)).
