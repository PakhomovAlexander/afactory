# Resolve Gate caches through machine-local bounded policy

**Status:** proposed

ADR-0008 requires safe Gate caches to be sandbox-local snapshots, but it does not define how a
project's symbolic cache request reaches an administrator-owned host subtree without making that
host path candidate-controlled execution authority.

Pipeline format v3 may request a closed symbolic cache kind under `[gate]`, initially only
`caches = ["cargo"]`. The pipeline never names a host path. Afactory resolves each requested kind
through a versioned machine-local policy at `$XDG_CONFIG_HOME/afactory/caches.toml` (or the exact
absolute file selected by `AFACTORY_CACHE_POLICY_FILE`). No mapping means no cache: a requested
kind fails before Gate command dispatch rather than falling back to an ambient package cache.

One policy entry names an absolute real source directory and positive byte, filesystem-entry, and
plain-copy limits, all below kernel ceilings. The initial Cargo layout admits only package
archives under `registry/cache/` and sparse-index data under `registry/index/`. It excludes
unpacked `registry/src/`, Cargo Git dependency caches, configuration, and credential-shaped
paths; symlinks and special files are also refused. The entry limit counts directories as well as
files, and traversal has fixed depth and path-size limits.

The kernel preflights through descriptor-relative no-follow opens and retains each regular-file
descriptor, so replacing a path after admission cannot redirect a read. Copying and hashing read
at most the admitted size plus one change-detection byte. After preflight, the kernel selects one
method for the whole snapshot: reflink when a retained-source-descriptor-to-sandbox probe
succeeds, otherwise plain copy only while the explicit copy limit covers the complete preflight
size. It never silently changes method partway through a snapshot.

Materialization happens after Gate-provider admission and before command dispatch under the
reserved sandbox path `.af-cache/cargo`. The check receives an exact local/container path through
`CARGO_HOME` plus `CARGO_NET_OFFLINE=true`. Cache writes remain inside that Gate clone. The kernel
removes the reserved cache subtree before sealing, so seeded or mutated cache bytes cannot become
Subject mutations or later reviewer input.

Every successful materialization is preserved as a CAS receipt referenced by the Gate decision.
Its versioned `CacheManifest@1` records the closed kind, `percent_v2` path encoding, sorted paths,
content digests, and sizes. `RunReport@5` projects the Gate node, symbolic kind, manifest digest,
byte/file counts, and exact materialization method. Every requested Gate/cache identity has either
that receipt or a typed failure reason, including a cache not reached because Gate setup failed.
The machine-local source path is deliberately absent. Existing pipeline formats and
`RunReport@1`–`@4` remain permanent readers; a v3 Gate without cache requests retains the M6.1
`RunReport@4` behavior.

Machine policy is resolved lazily only when an unresolved Gate runs. Replaying a durable Gate
receipt does not reread the policy or require the original cache source to exist.

## Considered options

- **Pass through the operator's package cache.** Rejected by ADR-0008: it exposes live host state,
  commonly including credentials, and lets sandbox writes persist after the run.
- **Put absolute cache paths in project pipeline authority.** Rejected because a repository would
  select host filesystem reads, leak machine topology into captured policy, and cease to be
  portable.
- **Discover `~/.cargo` implicitly.** Rejected because absence or a surprising credential file
  would silently change the review boundary; a safe cache is explicit administrator policy.
- **Ingest the entire cache into the Review Kernel CAS.** Rejected for this slice because it
  duplicates potentially large package archives in durable Campaign state. The receipt stores a
  bounded content-manifest digest without retaining the cache bytes.
- **Try reflink per file and silently copy failures.** Rejected because a large cross-filesystem
  cache could degrade into an unplanned multi-gigabyte copy after work has started.
- **Machine-local bounded policy plus one preflighted snapshot method (chosen).** This keeps host
  paths out of project authority, makes cost and credential boundaries explicit, and provides one
  replayable receipt for the exact bytes made available to the Gate.

## Consequences

- Cache use is opt-in twice: project authority requests a symbolic kind and machine policy maps
  it. Neither side alone grants host-cache access.
- A cache miss, invalid source, concurrent source change, unsupported reflink above the copy cap,
  or exceeded byte/file limit fails before an offline Gate can be treated as passing.
- The initial implementation supports Cargo only. Additional package managers require a new
  closed kind with fixed target and offline environment rules; arbitrary environment injection is
  not part of cache policy.
- Cargo Git dependency caches and broader Cargo-home layouts are postponed. They require their
  own layout-aware admission rules; M6.2 does not weaken credential refusal to make them fit.
- Cache acquisition and refresh remain separate trusted operator actions. A review never enables
  network access to repair an incomplete snapshot.
- The receipt proves what cache content entered one Gate sandbox, not that another machine retains
  those bytes. A later Round may resolve a newer cache and records a different digest.
