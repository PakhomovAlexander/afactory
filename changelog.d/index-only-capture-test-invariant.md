- The worktree-capture regression `capture::an_index_change_alone_invalidates_a_pass` now proves
  what its name says. Its repository keeps its administrative directory, the real index included,
  beside the worktree (`git init --separate-git-dir`), outside the root the capture's watcher
  monitors. The staged file is in the worktree before the capture starts, and only `git add`
  runs between the passes. The test asserts that the index lies outside the worktree, that every
  entry under the worktree keeps the same mode, bytes, and inode and time stamps across the
  staging, that `git ls-files -s` did change, and that the capture took exactly two attempts.
  The fence-and-probe settle wait and its scripted test are removed: they declared the watcher
  settled on an empty queue, which cannot prove that no repository callback is still coming.
  Capture itself is unchanged.
