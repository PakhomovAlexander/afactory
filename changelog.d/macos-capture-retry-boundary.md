- Fix macOS capture retries invalidated by delayed filesystem events. An attempt-local root
  vnode watch and complete metadata observations preserve mutation, ABA, index and metadata
  checks with the existing attempt and boundary budgets. Descriptor use is independent of
  tree size and depth, and no-follow descriptor-relative traversal prevents directory
  replacement from redirecting observation outside the source. Exhausted resources fail closed.
  See [the decision](docs/adr/2026-11-06-use-vnode-boundaries-for-macos-capture.md).
