- Claude Task Workers no longer fail with `Native billing usage is incomplete` on Claude Code
  2.1.285, whose final result counts requests in `modelUsage` that its top-level `usage` leaves
  out. A breakdown that no top-level counter exceeds is charged as the complete bill; a top-level
  counter above the breakdown is still refused
  ([ADR-0125](docs/adr/0125-charge-a-claude-model-breakdown-that-covers-the-top-level-summary.md)).
- An Attempt refused for incomplete billing now names the cause in its diagnostic, for example
  `Native billing usage is incomplete: Claude top-level and per-model usage cannot be reconciled`.
- A Claude Code update in the middle of a Task no longer refuses the Task's next Claude Worker
  with `Captured Task Provider identity is no longer current`. The Task keeps running the client
  executable it captured when it started; a captured executable that was removed is refused with
  its own message
  ([ADR-0126](docs/adr/0126-keep-the-captured-native-executable-when-its-launcher-moves.md)).
