- Require finite aggregate disk limits for Worker development environments. Version 3 companions
  and machine bindings explicitly authorize one private tmpfs filesystem for all writable roots
  and temporary areas; unsupported enforcement refuses dispatch. Bounded exports and dead-owner
  recovery preserve source, cache and reviewer isolation. See
  [the storage decision](../docs/adr/2026-11-04-bound-worker-storage-in-one-private-tmpfs.md).
