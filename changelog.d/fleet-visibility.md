- Add read-only fleet visibility through `af infra` and `af cluster`. Pinned management contexts
  show host headroom and allocation state. Text and JSON distinguish unknown storage pool
  inventory from a known empty inventory. Obsolete cache records are ignored during broker
  outages. Independently authorized Project readers show waiting Tasks and their pending Decisions.
  [Fleet visibility decision](../docs/adr/2026-11-25-observe-fleet-and-project-origins-with-pinned-readers.md)
- Exercise Fleet SSH authentication on macOS with disposable loopback endpoints that support
  Apple's audit-session setup.
- Read Project origins with read-only Store permissions after clean shutdown and while
  committed Task events remain in the WAL, without creating sidecars in the Stores.
- Keep fleet observation snapshots private to the origin reader, including the copied
  database and WAL, even under a permissive umask.
- Show reported disk charges for stopped VMs, preserve unknown broker values, and let each
  management read finish independently under the shared deadline.
- Count retained infrastructure observations against the machine storage budget and reclaim
  expired caches and abandoned private origin snapshots.
- Preserve the standalone wake account lookup and identity mismatch diagnostics when sharing
  OS account resolution with Project origin readers.
- Require the pinned OpenSSH fixture in fleet acceptance.
