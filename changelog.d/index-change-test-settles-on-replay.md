- The worktree-capture regression `capture::an_index_change_alone_invalidates_a_pass` no longer
  hangs for its 120-second deadline on a loaded macOS host. FSEvents there drops events, stalls,
  and replays a path's earlier events to a newly started watcher, so the test stopped waiting
  for the index rename to be reported. It now waits until a watcher started before the change
  reports a fence file written beside the repository after it, and then until two fresh probes
  watching the repository, as the next capture pass's watcher does, each report their own fence
  and no replayed event. The test still asserts exactly two attempts within the same 120-second
  bound, and capture itself is unchanged. `CHANGELOG.md`'s second H1
  (`# Performance laboratory whole-source repairs`) is a plain lead-in line again, so
  markdownlint's MD025 passes.
