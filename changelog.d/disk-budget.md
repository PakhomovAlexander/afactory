- af now keeps a bounded, visible amount of disk
  ([ADR-0144](docs/adr/0144-hold-afs-disk-use-to-a-machine-budget.md)). A machine-only
  `[storage]` table (20 GiB `max_bytes` and a 10 GiB `min_free_bytes` floor by default;
  `AF_STORAGE__<KEY>` overrides it, and a repository's `.af/af.toml` cannot) bounds warm build
  keys, warm Workspaces, review campaigns, Task Stores and installed versions, evicting the least
  recently used entry first and never one in use or used within the hour. Collection runs after
  every `af task run` and `af review run` (`auto_gc`), reaching Stores this release cannot read
  and Stores made with `--state`, which `$XDG_STATE_HOME/af/stores.toml` now records. Below the
  floor a check reports `insufficient_disk` and a Worker Attempt is refused before any token is
  spent. `af storage` shows what af holds and `af storage prune [--apply]` reclaims it. A warm
  check with a native toolchain mapping now reuses one key instead of making a new one every
  Attempt (the key domain moves to `af.task-build-cache.toolchain/2`, so old keys are evicted
  once). Every check, review gate checks included, gets its own empty `HOME`, `TMPDIR`,
  `AF_CHECK_SCRATCH` and `XDG_CACHE_HOME`, removed after it: write to `$AF_CHECK_SCRATCH` or
  `$TMPDIR`, never `/tmp`. af removes each Claude Worker Attempt's history from the Claude config
  directory (`keep_worker_transcripts` keeps it), and closes a finished Task's gate pull request
  and deletes its `af-gate/` branches (`keep_gate_pull_requests` keeps them), recording the
  result as `gate_cleanup` without changing the Task's result.
