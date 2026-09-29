- A live Task writer no longer fences itself with `Task writer lease is expired or fenced` on a
  loaded machine. The lease heartbeat now observes the lease through its own Store connection.
  It renews through that connection when the work still holds the shared one with 2 s of lease
  left, so a slow Store operation cannot outlast the lease. A successor or an expired lease
  still fences and cancels the old writer, and is now seen without waiting for the held Store
  ([ADR-0125](docs/adr/0125-renew-a-live-task-writer-lease-through-its-own-connection.md)).
