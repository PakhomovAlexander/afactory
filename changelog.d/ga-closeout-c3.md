- The Storage Budget keeps every Task a delivered Workspace was made from. The delivery records the
  Task ID as data in a new Project Store event, `WorkspaceDelivered@1`, so the protection holds
  when the label was suffixed because the Task ID was taken. Every Project Store is read for this
  without the discovery ceiling. While one cannot be read, a sweep collects no finished Task and
  names that Project Store. A campaign whose writer leases cannot be read is in use for the sweep
  and `af review gc`
  ([ADR-0158](../docs/adr/0158-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md)).
- `af storage --json` prints `af/storage@5`. Its `volumes` name each root the free-disk floor
  measures (`sandbox`, `cache`, `build_cache`) with its volume, `free_bytes`, `below_floor` and
  `next_action`. Its `unreadable_store` entries carry the `writer_release` and the `next_action`
  from the Store's provenance. A version-1 `stores.toml` is replaced by the first write that
  meets it, said once, instead of a warning on every run.
- `af self update --check --rc` on the stable channel records the pre-release only as
  `latest_prerelease`, and status keeps naming `af self update --rc`. A failed explicit check is
  recorded as the last attempt with its cause. `af self status --json` prints `af/self-status@2`,
  with `check_lock` and `next_action` (`rm <lock>` for a stalled check). The update row of
  `af doctor` warns of a failed or stalled check with the same fix. On macOS its storage row warns
  when Spotlight indexes the warm build cache, the warm Workspaces or the sandbox root, and
  [docs/README.md](../docs/README.md#macos-spotlight-and-gatekeeper) gives the exclusion.
- `af provider detect` prints `af/provider-detect@2`. Each auth probe waits 10 seconds at most,
  and an unknown login carries `auth_cause` and `auth_detail`. A directory that cannot be listed
  is probed by its marker names. An unreadable registry makes every row's `registration`
  `unknown`, and `--apply` refuses it with exit 1. `--apply` checks a registered row again under
  the registry lock, and leaves a login whose CLI is off `PATH` unregistered with the PATH line
  (exit 4). A conflict names `conflict_with` and suggests the retry with a free ID. The closing
  `next:` line follows the outcome, and `--kind claude` gives no Codex advice
  ([ADR-0206](../docs/adr/0206-detect-provider-logins-and-register-them-through-add.md)).
- `af provider status --json` prints `af/provider-status@3`, with `found_unregistered` and
  `next_action`. `af doctor` and `af onboard` list the same directories from the same scan. A
  quota window without a reset time says `Claude does not report when this window resets`
  (`reset_reported: false`). The browser's ambient row now says that unsetting `CODEX_HOME` lists
  `~/.codex` instead, rather than that it stops the listing.
- A Claude reply that is the account's usage-limit refusal now says `the Claude account's usage
  limit is reached; it resets at <time>` (cause `provider_capacity`). It no longer reports the
  internal model-bound error. `make prereqs` reports rust missing when the pinned toolchain lacks
  `clippy` or `rustfmt`, with the `rustup component add` command. Its report wraps at 100 columns
  and spells `$HOME` and `$TMPDIR` short.
