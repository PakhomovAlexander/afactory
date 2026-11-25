- Add read-only fleet visibility through `af infra` and `af cluster`. Pinned management contexts
  show host headroom and allocation state. Text and JSON distinguish unknown storage pool
  inventory from a known empty inventory. Obsolete cache records are ignored during broker
  outages. Independently authorized Project readers show waiting Tasks and their pending Decisions.
  [Fleet visibility decision](../docs/adr/2026-11-25-observe-fleet-and-project-origins-with-pinned-readers.md)
- Exercise Fleet SSH authentication on macOS with disposable loopback endpoints that support
  Apple's audit-session setup.
- Read Project origins with read-only Store permissions after clean shutdown and while
  committed Task events remain in the WAL, without creating sidecars in the Stores.
