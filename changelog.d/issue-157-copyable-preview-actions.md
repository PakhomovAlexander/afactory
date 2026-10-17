- `af task start` and `af task explain` no longer wrap a suggested `af task run` action or
  `Details` hint inside a Task ID. With a 128-character Task ID, the copied command keeps every
  shell token, the exact `--confirm-plan` ID and a valid `\` continuation. Prose rows still wrap at
  96 columns, and command rows are still sanitized
  ([ADR-0149](docs/adr/0149-never-wrap-a-suggested-command-inside-a-shell-token.md)).
