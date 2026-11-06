# ADR-2026-11-06-use-vnode-boundaries-for-macos-capture: Bound macOS capture with vnode watches

**Status:** proposed (2026-11-06); accepted on merge.

## Context

Issue #697 exposed macOS FSEvents delivering setup or previous-attempt activity during a
stable capture retry. Equal index and worktree fingerprints then took three attempts instead
of the required two. The delayed observer reproducer and original assertions remain unchanged.
notify 8.2.0 starts its stream with `SinceNow`, discards native event IDs and exposes no flush
operation. Retaining one stream and draining delivered batches did not fix delayed delivery.

## Considered options

- Longer sleeps, more attempts, or accepting equal scans despite mutation notifications would
  weaken the boundary or its budgets; reject them.
- A shared FSEvents monitor still receives delayed notifications across retry boundaries;
  the previous native experiment failed.
- Flushing FSEvents requires a native stream handle that notify does not expose. Adding a
  private unsafe FFI implementation or a vendored notify fork is a larger change.
- notify's kqueue backend opens paths for reading, follows links and rejects dangling links.
  Capture must preserve these links without reading their targets.

## Decision

On macOS only, use nix's safe kqueue interface for an attempt-local recursive vnode monitor.
Open each vnode with `O_EVTONLY | O_SYMLINK`, register its mutation flags synchronously and
check its registration receipt. Register a directory before enumerating its children. Keep
all descriptors alive through both scans and query pending kernel events directly at close.
A parent notification invalidates creation, deletion or replacement even when the new path
was absent from the initial watch set. Each retry creates a fresh queue, so earlier activity
cannot replay into it. Other platforms retain their existing notify backend.

macOS also reports read-induced access-time updates as `NOTE_ATTRIB`. An attribute-only
notification is dismissed only when the watched descriptor still has its registration-time
device, inode, modification/change timestamps, mode, owner, group, link count and size.
Other flags always invalidate; metadata disagreement invalidates even if bytes were restored.
The unchanged two scans independently compare content, kinds, sizes and change stamps, with
index fingerprints before and after. Access notifications never renew the 250 ms deadline;
an event storm that exhausts it fails closed. Syscall and registration errors also fail closed.

## Consequences

The three-attempt limit, 250 ms close budget, exact settled-content proof and failure checks
remain in force. Capture adds no writes to the source and follows no symlink targets when
building its watch set. It holds one descriptor and metadata record per watched path, including
ignored paths and Git administration. Descriptor exhaustion refuses capture and releases the
partial watch set; there is no raised machine limit, polling fallback or partial monitoring.
Native regression tests cover retries, transient creation, metadata ABA, link targets,
registration receipts, read access and resource exhaustion. Native execution is required to
establish watcher correctness; command-sandbox passes alone are insufficient.
