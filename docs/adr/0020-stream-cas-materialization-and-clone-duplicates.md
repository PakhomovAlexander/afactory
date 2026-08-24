# Stream CAS materialization and clone duplicate files

**Status:** proposed

Snapshot objects may be larger than memory and one digest may occur at thousands of manifest
paths. Materialization runs on the shared infrastructure executor from ADR-0018, so a worker must
not park waiting for memory held by another task or start a nested fan-out whose progress depends
on the outer task.

We decided to group entries by digest in linear time while preserving first-occurrence order, then
run two non-nested executor phases. The source phase reflinks each CAS object into a sibling
temporary file when the filesystem supports copy-on-write, hashes the cloned file through a fixed
64 KiB buffer, and atomically publishes it only after successful verification. A cross-device or
non-COW filesystem falls back to streaming the CAS object into that private temporary file through
the same verification buffer. The duplicate phase reflinks further regular-file occurrences from
those verified sources, with a plain-copy fallback, and gives each occurrence its own executable
mode. A heavily repeated digest can therefore use the whole executor without making one worker wait
for work it submitted. Symlink target bytes are read once only when the group contains symlinks and
are refused above a fixed 16 KiB limit before allocation. Every path is decoded once, and a serial
walk over the manifest's sorted paths prepares newly encountered parent components before either
parallel phase; each existing component must be a real directory, never a symlink.

## Considered options

- **Read each CAS object into a byte vector.** Simple and fast for small objects, but resident
  memory becomes workers times object size. Rejected because candidate content controls object
  size.
- **Admit whole-object reads through a condition-variable byte budget.** Bounds bytes in one
  materialization call, but parks shared executor workers and fails to compose across concurrent
  calls. Rejected because the executor itself is the progress boundary.
- **Keep duplicate writes serial inside one digest task.** Avoids nested work, but a single heavily
  repeated digest uses one worker while independent destinations remain. Rejected after dogfood
  demonstrated the duplicate count itself is a material workload.
- **Use nested parallel writes for heavily repeated digests.** Uses more workers, but progress
  depends on re-entrant scheduling and needs an arbitrary group threshold. Rejected because an
  outer worker must not wait for work it submitted to the same bounded executor.
- **Always stream-copy verified sources, then clone duplicates.** Bounds memory and atomically
  publishes only verified sources, but writes every distinct snapshot byte even when source and
  materialization root share a COW filesystem. Retained as the portability fallback.
- **Reflink and verify sources, then clone duplicates in a second phase (chosen).** Preserves the
  streaming design's integrity and memory bounds, avoids copying distinct bytes when COW is
  available, performs no blocking admission or nested scheduling, and exposes every duplicate
  destination to the shared worker budget.

## Consequences

- Regular-file content memory is bounded by the executor worker count times 64 KiB; symlink
  targets are the only whole objects retained in memory and are capped at 16 KiB each.
- A corrupt CAS object may leave unverified bytes only in an unlinked sibling temporary file; the
  declared target path is never published and no sandbox is admitted.
- On a COW filesystem the template's regular-file sources share blocks with the immutable CAS;
  fallback filesystems pay one fixed-buffer copy per distinct digest.
- Distinct source digests and duplicate destinations are separately parallel. Their phase boundary
  guarantees every clone source has already been verified without nested executor work.
- Materialization verifies the authoritative CAS bytes before clones become sources for further
  occurrences and validates every manifest-declared size against the verified byte count.
