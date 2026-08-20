# Safe caches are sandbox-local snapshots

**Status:** accepted (2026-08-20); supersedes
[ADR-0003](0003-gate-caches-pass-through-to-the-host.md)

A safe Gate may use a dependency cache, but it may not directly map an operator's host cache.
Administrator policy maps symbolic cache kinds to explicit credential-free subtrees; the kernel
materializes a bounded copy or copy-on-write clone inside the sandbox and records its source
digest, size, and materialization method. A direct host passthrough is available only to an
explicitly unsafe `trusted_local` execution policy and cannot satisfy container isolation.

## Considered options

- **Root-allowlisted host passthrough.** Rejected after acceptance in ADR-0003: `~/.cargo` contains
  credentials, read-only mappings still expose secrets, writable mappings create persistence,
  and claiming unchanged isolation after either mapping makes the policy label unreliable.
- **Always copy the whole cache.** Rejected because a cross-filesystem multi-gigabyte copy can
  stall a review and because whole cache roots include files a check does not need.
- **Bounded credential-free Cache Snapshots (chosen).** They preserve warm dependencies without
  giving the check a live path to host state.

## Consequences

- Cache materialization has explicit byte/file limits and preflights reflink support. Exceeding a
  limit or requiring an unbounded fallback fails with a clear diagnostic.
- Safe gates run package managers offline. A missing object is a cache miss, not permission to
  fetch from the network.
- Cache writes are sandbox-local and discarded. A separate trusted acquisition step may update
  the administrator-owned source cache outside the review run.
