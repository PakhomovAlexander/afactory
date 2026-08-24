# Stream CAS materialization and clone duplicate files

**Status:** proposed

Snapshot objects may be larger than memory and one digest may occur at thousands of manifest
paths. Materialization runs on the shared infrastructure executor from ADR-0018, so a worker must
not park waiting for memory held by another task or start a nested fan-out whose progress depends
on the outer task.

We decided that each digest group is one executor task. Its first regular-file occurrence is
streamed from the CAS through a fixed 64 KiB verification buffer. Further regular-file
occurrences are reflinked from that verified file, with a plain-copy fallback, and receive their
own executable mode. Symlink target bytes are read once only when the group contains symlinks and
are refused above a fixed 16 KiB limit before allocation.
Parent paths are created component by component immediately before each write and every existing
component must be a real directory, never a symlink.

## Considered options

- **Read each CAS object into a byte vector.** Simple and fast for small objects, but resident
  memory becomes workers times object size. Rejected because candidate content controls object
  size.
- **Admit whole-object reads through a condition-variable byte budget.** Bounds bytes in one
  materialization call, but parks shared executor workers and fails to compose across concurrent
  calls. Rejected because the executor itself is the progress boundary.
- **Use nested parallel writes for heavily repeated digests.** Speeds plain copies on some
  filesystems, but makes progress depend on re-entrant scheduling and needs an arbitrary group
  threshold. Rejected because reflink/copy already handles duplicates without a second task set.
- **Stream one verified occurrence and clone duplicates (chosen).** Bounds memory independently
  of object size, performs no blocking admission, and preserves one task per digest.

## Consequences

- Regular-file content memory is bounded by the executor worker count times 64 KiB; symlink
  targets are the only whole objects retained in memory and are capped at 16 KiB each.
- A corrupt CAS object may leave a partial file only inside the disposable materialization root;
  the operation still fails and no sandbox is admitted.
- Duplicate occurrence writes within one digest group are serial, while distinct digests remain
  parallel on the shared executor.
- Materialization verifies the authoritative CAS bytes before clones become sources for further
  occurrences and validates every manifest-declared size against the verified byte count.
