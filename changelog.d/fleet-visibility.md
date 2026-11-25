- Add read-only fleet visibility through `af infra` and `af cluster`. Pinned management contexts
  show host headroom and allocation state. Storage pool inventory is unknown when the broker
  reports host-level SSD headroom only. Independently authorized Project readers show waiting
  Tasks and their pending Decisions.
  [Fleet visibility decision](../docs/adr/2026-11-25-observe-fleet-and-project-origins-with-pinned-readers.md)
- Exercise Fleet SSH authentication on macOS with disposable loopback endpoints that support
  Apple's audit-session setup.
