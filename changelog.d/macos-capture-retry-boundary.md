- Fix macOS capture retries invalidated by delayed filesystem events. Attempt-local vnode
  watches preserve mutation, ABA, index and metadata checks with the existing attempt and
  boundary budgets; exhausted watch resources fail closed.
  See [the decision](docs/adr/2026-11-06-use-vnode-boundaries-for-macos-capture.md).
