# ADR-2026-11-04-bound-worker-storage-in-one-private-tmpfs: Bound all Worker filesystem writes

Status: proposed; accepted on merge.
Date: 2026-11-04.

## Context

A Worker could omit `disk_bytes` and receive writable host binds. Wall deadlines and immutable
cache capture limits did not constrain bytes allocated during execution. This decision supersedes
the disk-refusal and writable-bind portions of
[the initial environment decision](2026-11-03-worker-development-environments.md), and advances
the companion and machine-binding versions introduced by
[the cache decision](2026-11-04-carry-declared-worker-caches-in-exact-warm-sets.md).
Language-neutral harnesses, cache provenance, knowledge visibility, reviewer independence and
Gate authority remain the same.

## Considered options

- Scan after execution, poll filesystem sizes, or apply per-file limits: rejected because none
  prevents aggregate allocation between checks or across files and roots.
- Keep writable host binds with optional disk limits: rejected because an omitted limit leaves
  a Worker able to exhaust the host filesystem.
- Disable development Workers: rejected because local compilation and private reproductions
  are required capabilities.
- Grant a Worker mount privileges or expose the daemon socket: rejected because either widens
  Worker authority beyond its declared private environment.
- Divide independent quotas among each root: rejected because arbitrary fixed partitions waste
  capacity and do not express the requested shared aggregate bound.

## Decision

`af.worker-development/3` requires a positive aggregate `disk_bytes` bound, an exact 4096-byte
multiple through 8 GiB. `af.worker-development-binding/3` requires its own finite ceiling and
explicit `storage = "tmpfs_volume"` authority. Both earlier versions fail closed. A machine
binding authorizes local-driver tmpfs volume operations and fixed image-only storage helpers;
it grants no Worker capability, network, credential, host cache or writable host mapping.

One new private tmpfs volume holds source, Base, state, scratch, build, cache, knowledge and
auxiliary writable directories for an Attempt. Initial captured bytes consume the same capacity.
A fixed helper verifies the actual filesystem type, block size and exact capacity, then copies
read-only inputs. The runtime must support local tmpfs volumes and volume subpath mounts;
unavailable enforcement refuses dispatch. A fixed inode ceiling also bounds filesystem metadata.

The Worker sees only separate volume subpaths, with undeclared roots and Base read-only. Its
`/tmp`, `/run` and `/dev/shm` use this same filesystem. It receives a read-only image and package,
no writable host binds, no capabilities and a non-root UID. The root-owned `/dev` is unwritable;
`/dev/mqueue` is covered by a read-only empty directory. IPC sharing and daemon logging are
disabled. Writes to any enabled root compete for the same kernel-enforced filesystem capacity.

The helper reads private inputs as the operator UID. For a root operator only, explicit backend
authority permits `CHOWN` on the fixed seed helper, whose only writable mount is the private
volume, to transfer those files to UID 65534. The Worker still has no capability. Non-root
operators need no ownership capability and use their own non-root UID for the Worker.

After the workload's container and descendants stop, a read-only helper exports declared writable
roots through the existing supervised streaming boundary. A closed archive reader stages only
regular files and directories, checks aggregate logical bytes, entries, depth and canonical path
components, and restores only private permissions and executable bits. Bounded GNU long-name
records preserve filesystem paths through 4096 bytes; arbitrary extensions, links, devices,
traversal, duplicates and oversized sparse files are refused. All roots validate before any input
is replaced. A failed export or replacement fails the Attempt and cannot supply a cache or final
source receipt. Final Manifests describe the exported bytes; actual harness output remains exact.

CheckRuntime owns a synced, strict `af.worker-storage/1` intent before any daemon mutation.
A bounded keeper holds the volume mounted across seed, workload and export. Cleanup removes
exactly that intent's helper/workload/keeper names and volume in the private local daemon context.
The existing dead-owner sweep recovers the intent before removing the runtime. Live owners and
preserved roots remain untouched; unsupported intents and unconfirmed cleanup are retained.
Existing CAS publication, Task leases, reachability and Storage Budget own retained cache bytes.

## Consequences

Configured command Workers remain usable with real toolchains, private repeated-command state
and independent reviewer experiments. Operators review new binding/package pins rather than
silently retaining an old unbounded environment. Copies and exports add bounded work only for
declared development environments; undeclared Workers and Gate dispatch retain their paths.
The backend requires an available compatible local Engine and fixed GNU image utilities.

Live container probes allocate across every Worker root and auxiliary writable area until
`ENOSPC`, check an independent concurrent Worker and untouched host marker, refuse sparse export
amplification, and recover a mounted volume after killing its exact owner process. Existing
Task-dispatched Rust/Python and mid-capture/clone crash regressions remain required. These are
containment and functional evidence, never performance measurements or independent acceptance.
