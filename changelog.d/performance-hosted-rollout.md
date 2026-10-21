### Fixed

- Admit the pinned artifact action's flat single-download layout for performance shards and
  reports, retaining run/plan identity checks and missing-shard failures. Run benchmark CLI
  setup, samples and diagnostics with `AF_SELF_OFFLINE=1`, preventing a detached update check
  from invalidating otherwise completed fixture setup.
