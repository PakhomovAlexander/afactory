- `af` checks for a newer release after eligible headless and `--json` commands too, not only on a
  terminal: a detached `af self refresh-check`, at most once per `[self] check_every`, a failed
  query or a self state that cannot be written included, that never changes the command's output
  or exit status, and that a command whose versions lock is held by a prune, an installation or
  an uninstall skips rather than waits for. Inside a pinned project
  the dispatching `af` schedules it once. The cached notice still reaches stderr only on a
  terminal and never under `--json`, and clearing it never overwrites a concurrent check's cache;
  a check that another one answered while it waited asks nothing, and an installation, `always`
  included, no longer holds the versions lock while it downloads, so a pinned command never waits
  for it, nor does `always` overwrite a default the operator changed meanwhile, an update then a
  rollback to the same version included; a seen pin is never recorded without the versions lock;
  `af self remove` and `af self prune` recheck the default and the pins under that lock and delete
  under it, so a concurrent activation never leaves the default pointing at a deleted release; an
  "updated the default" notice a later update or rollback made stale is cleared unshown; a pinned
  command on a terminal shows the cached notice before it dispatches, and the notice is bound to
  the activation that made it, so an update back to the same version after a rollback drops it too
  (the cache is now `af/self-check@2`; an earlier release's cache is read as none);
  `af self uninstall`, with or without `--purge`, refuses while a background check runs, and
  neither a check nor an installation that was under way recreates what it removed; a dispatch
  rechecks the pinned release's bytes and records its pin in one versions-lock critical section,
  so `af self remove` or `prune` can no longer delete it in between, and runs the release only once
  that pin is written, failing closed when the self state or the versions lock cannot be used; a
  command still running when `af self uninstall --purge` ran recreates nothing it removed; an
  "updated the default" notice is printed only once clearing it is written, so a failed write never
  shows it twice; a killed installer's staging
  counts
  against the Storage Budget until the next installation recovers it; `CI`, `AF_SELF_OFFLINE`,
  `update_check = false` and `auto_update = "never"` still turn the check off ([ADR-0154](docs/adr/0154-check-for-updates-after-headless-and-json-commands.md)).
