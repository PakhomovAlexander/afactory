- Security audit 2026-10-20, state, storage and policy parsing (H2, M6, M5, L10, L14, L4, I7). An
  explicit `--state` directory must be absent, empty or already an af Store: one holding anything
  else is refused before anything is written or registered, and a new Store carries the owner
  marker `.af-store`. The sweep, `af storage prune --apply`, `af review gc --state-root` and
  `af self uninstall --purge` remove a Store outside af's roots whole only while it is marked and
  holds nothing but af's entries, each known by its exact name (`notes.lock`, `backup.lock` or
  `events.sqlite.notes` is never af's); otherwise they remove af's entries alone and name what they
  left (`left` in `af/storage-prune@1`), and such a Store counts only af's entries against the
  budget. af's state and telemetry directories are created `0700` and its files there `0600`
  (the SQLite `-wal` and `-shm` included) whatever the umask, and an open af-owned root is
  narrowed to `0700`; the checkout keeps the user's umask. Route patterns match in
  O(pattern × path) and are capped at 1024 bytes and 64 wildcards. Authority files are read
  by the path as named, never canonicalised first, without following a final link and within
  1 MiB each: a linked configuration layer is refused in one line, and a project's `.af/af.lock`
  that dispatch or the remembered pins read is refused, in one line on stderr, when it is a link
  or over 1 MiB, Worker packages within 256 files and 8 MiB,
  and a parse error quotes at most 200 bytes of its line. A Task cancelled or abandoned after its
  gate published is listed and cleaned up like a finished one. A Campaign label is at most 128
  bytes and refuses format, bidi-control and separator characters
  ([ADR-0144](docs/adr/0144-hold-afs-disk-use-to-a-machine-budget.md),
  [ADR-0035](docs/adr/0035-address-campaign-state-by-opaque-id.md)). Outside this package's
  files, minimal edits: the `--state` help in `crates/af/src/cli.rs`, the Task Store creation
  and `--state` admission in `crates/af/src/task_execution.rs`, the private `git-home/` in
  `crates/af/src/task.rs`, and the `nix` features of `review-store` and `review-telemetry`.
