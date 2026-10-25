- The release check cannot stall: `curl` runs with `--connect-timeout 5 --max-time 30` (its
  retries within the limit), `gh` under a 30 s deadline, and af ends a client that outlives
  them. The cache is `af/self-check@3`: it records the last attempt and why it failed, which
  `af self status` prints (`last check: <time> · last attempt <time> failed: <reason>`); a check
  that has held `latest.lock` for over an hour counts as none and is reported with `rm <lock>`.
  `af self status` schedules a due check itself, `af self status --refresh` runs it at once, and
  `--json` prints `af/self-status@1`. A pre-release af asks for pre-releases whatever the channel
  and hears `latest stable X · latest pre-release Y (you run R: af self update --rc)`; a release
  older than the running one is never shown as simply `latest`. `af self update --check` honours
  `AF_SELF_OFFLINE` (`offline: check skipped`, exit 0) and says the anonymous route's HTTP status
  or curl error before any `gh auth login` hint; neither it nor `install.sh` needs `gh` for a
  public repository ([docs/self-management.md](../docs/self-management.md)).
- The Storage Budget keeps what it ate before: the newest warm key of a project checked in the
  last 7 days is never evicted, nor a key a check holds; caches go before telemetry and records;
  a finished Task younger than `[storage] keep_days` or behind a delivered Workspace stays; a
  Store with a live writer lease is judged in use before the sweep plans; a Project Store another
  release wrote is never collected, with or without Workspaces (#302). The default `[storage]
  max_bytes` is `"auto"`: 20 GiB, or 40 GiB when the cache volume has more than 100 GiB free, and
  `af storage` and the run say once when a warm key does not fit the budget, with the budget that
  does.
- A Task command prints one sweep line, `storage: freed 1.2 GiB (2 warm keys)`, instead of every
  `af storage:` removal; `af task run --verbose` and `af task start --execute --verbose` print
  them all. `insufficient_disk` names the volume by mount point and type with both numbers and
  the way out (`/tmp (tmpfs): 1.9 GiB … free, floor 10.0 GiB …; the sandbox root is /tmp; set
  TMPDIR or [storage] sandbox_root to a volume with room`); `[storage] sandbox_root` and
  `build_cache_root` move sandboxes and warm keys, and the floor measures those volumes.
- The Rust toolchain mapping is configured: `[task] rust_toolchain_policy` in the user's
  configuration or a project's `.af/af.toml`, with `AF_TASK_RUST_TOOLCHAIN_POLICY_FILE` as the
  override, and `af config show --origin` names where it came from. `af self optimize --execute`
  runs under the interrupt watcher like every Task-running command (#158). `make prereqs` checks a
  contributor's machine, one line per tool with its install command, and changes nothing (#161).
