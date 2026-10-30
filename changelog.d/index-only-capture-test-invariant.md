- The worktree-capture regression `capture::an_index_change_alone_invalidates_a_pass` now proves
  what its name says. The staged file is in the worktree before the capture starts, and only
  `git add` runs between the passes. The test then asserts that both passes read the same paths,
  kinds, bytes and inode and time stamps, that the index did change, and that the capture took
  exactly two attempts. Its settle wait no longer counts a probe as clean when the queue is empty
  just after the probe's fence callback. Each probe now writes a second fence beside the repository
  only after the first is reported, and is clean only if no repository event reaches it before
  or with that second fence. A repository callback delivered after the first fence now restarts
  the wait, which keeps its 120-second bound. The new
  `capture::a_repository_callback_after_the_fence_does_not_settle_the_probe` proves this on
  scripted deliveries, with no watcher and no waiting. Capture itself is unchanged.
