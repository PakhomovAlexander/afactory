- Bare `af` opens its browser before any Task or Campaign Store is read: the Tasks, Campaigns and
  Workers panes read their Stores on a thread behind a spinner on their folder, Store by Store,
  so a machine whose registry names dozens of Stores no longer sits on the splash for as long as
  every event log takes to replay (40 s measured; issue #402,
  [ADR-0240](docs/adr/0240-open-the-browser-before-any-store-is-read.md)).
- The user scope's walk of the state root for every repository's Store is that read's too, and a
  Store registered or `:state`-added while a Task or Campaign is opened never shows the opened
  entry as gone: the listing stands until the read answers for its Store.
