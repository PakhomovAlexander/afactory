- A live Task writer no longer fences itself with `Task writer lease is expired or fenced` on a
  loaded machine. The lease heartbeat now observes the lease through its own Store connection.
  It renews through that connection when the work still holds the shared one with 4 s of lease
  left. That connection waits at most 1 s for SQLite's write lock and retries with backoff, so
  neither a slow Store operation nor its write lock can outlast the lease. A renewal that
  commits after its lease expired is now refused inside the write transaction. A successor or an
  expired lease still fences and cancels the old writer, and is now seen without waiting for the
  held Store
  ([ADR-0128](docs/adr/0128-renew-a-live-task-writer-lease-through-its-own-connection.md)).
