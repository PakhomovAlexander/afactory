- The worktree-capture regression `capture::an_index_change_alone_invalidates_a_pass` no longer
  hangs for a 120-second deadline on a loaded macOS host, and no longer waits on filesystem
  notifications at all. FSEvents there drops events, stalls, and replays earlier events to a
  newly started watcher, and no wait on a watcher can prove that a later callback is not still
  coming. The test therefore no longer stages inside the watched worktree; see
  `index-only-capture-test-invariant.md`. Capture itself is unchanged. `CHANGELOG.md`'s second H1
  (`# Performance laboratory whole-source repairs`) is a plain lead-in line again, so
  markdownlint's MD025 passes.
