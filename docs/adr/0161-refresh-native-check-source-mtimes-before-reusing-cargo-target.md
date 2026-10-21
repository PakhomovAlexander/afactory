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
key (when eligible), but before its command can start, the kernel sets the modification time of every Manifest-declared regular source file
in that private tree to one timestamp two seconds ahead of the current clock. The lead makes
the source strictly newer than an artifact built moments earlier even on filesystems with
coarse mtimes; every stored mtime must be strictly later than the end of warm preparation, or
preparation fails closed. Refreshing before the lock wait is insufficient: the previous holder
could build another Snapshot while this check waits. The refresh opens each file through no-follow directory descriptors anchored at the private
root, refusing a symlink in any component and a non-regular replacement. Symlink entries are skipped. The CAS, source Manifest, source bytes,
and modes are unchanged. Cancellation and the Attempt's remaining deadline are checked before every entry, immediately
before every stamp, and before reporting the tree refreshed.
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
check scans and updates one metadata entry per regular source file; the two-second lead may
produce Cargo's normal future-mtime diagnostic until wall time catches up. No receipt or event
shape changes.
