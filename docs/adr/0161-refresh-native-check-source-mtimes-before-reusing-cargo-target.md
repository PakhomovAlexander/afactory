# ADR-0161: Refresh native check source mtimes before reusing a Cargo target

**Status:** proposed (2026-10-21)

## Context

[Issue #266](https://github.com/PakhomovAlexander/afactory/issues/266) records two Task
checks of different Snapshots sharing the toolchain-keyed `cargo_target` of
[ADR-0131](0131-warm-task-checks-through-a-toolchain-keyed-bounded-cache.md). A materialized
source can retain the modification time of its CAS blob through a reflink. Cargo compares source
mtimes with artifacts in the shared target. When a later check returns to older source bytes,
Cargo can treat an artifact built from another Snapshot as fresh. A check can then fail or pass
against code absent from its own Snapshot. A plain copy does not supply a reliable freshness
rule either.

## Considered options

- **Key the target by Snapshot.** Rejected: it creates a new large target for every candidate
  and loses reuse of registry dependencies and prior build output.
- **Record the last built Manifest and touch only changed paths.** Rejected for this fix: a
  failed or aborted check would need durable transactional cache provenance, and every changed
  persisted shape would need a new version. An incomplete record could attest a false pass.
- **Remove the target before each check.** Rejected: it makes the declared warm layer useless.
- **Refresh regular source mtimes in the private check tree (chosen).**

## Decision

After materializing each native Task check or measurement repetition and acquiring its warm
key (when eligible), but before its command can start, the kernel observes the newest entry mtime
in the bound Cargo target through a descriptor-relative, no-follow walk. Wall time alone cannot
order sources after a target: clock rollback or a prior command can leave future-dated artifacts.
When the observed target is ahead of the current clock, this invocation binds its private cold
target instead and records `cargo_target:future_mtime`, ineligible with zero available bytes.
Cargo home remains warm. The original target stays locked, bounded and monitored; it is neither
rewritten nor recursively removed on this deadline-sensitive path. It stays ineligible until
the clock catches up or ordinary cache eviction removes it.

The kernel sets every Manifest-declared regular source file in the private tree to one current
timestamp. Every stored mtime must be strictly newer than the observed target maximum, or the
whole tree is retried at a later current time under the same budget. This covers coarse mtime
granularity without a future lead that makes a later Cargo invocation rebuild the same units.
Before dispatch, no stored stamp may still be in the future; clock rollback during refresh
therefore waits within the same budget or fails closed. Refreshing before the lock wait is
insufficient: the previous holder could build another Snapshot while this check waits.
Each source is opened through no-follow directory descriptors anchored at the private root,
refusing a symlink in any component and a non-regular replacement. Symlink entries are skipped.
The CAS, source Manifest, source bytes and modes are unchanged. Cancellation and the Attempt's
remaining deadline are checked during target directory enumeration and traversal, before every
source entry and stamp, during every bounded wait, and before reporting the tree refreshed.
A timeout makes the check `not_run` or the measurement fail with its deadline reason; an I/O
error fails preparation. The command never runs with possibly stale build output.

The warm cache lock, key, bounds and check environment remain as in ADR-0131. This refresh
applies even when the target is cold or unavailable, so an aborted previous check requires no
recovery record. The existing warm observation reports bytes available before the check. It
does not claim a Cargo cache hit or name a previously built Snapshot; no such provenance is
recorded by this strategy.

## Consequences

Cargo rebuilds workspace units on each native check, including an A→B→A return to older
source content. Registry dependencies and the bounded shared target remain available. The
check scans the held target's metadata and updates one metadata entry per regular source file.
Multi-step checks can reuse workspace units built earlier in the same command. A future-dated
target costs a cold build, not a possibly stale result. No receipt or event shape changes.
