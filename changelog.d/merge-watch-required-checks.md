- Require successful exact-head CI checks before merge-watcher cleanup and `DONE`, including
  PRs already merged at startup or observed merging during a poll. Missing or failed checks
  preserve configured worktrees and branches until the checks pass.
