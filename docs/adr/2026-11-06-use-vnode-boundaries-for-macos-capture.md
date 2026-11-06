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

On macOS only, retain one root directory descriptor opened with `O_DIRECTORY | O_NOFOLLOW |
O_CLOEXEC` and one attempt-local kqueue vnode watch. Check the synchronous `EV_RECEIPT`
registration result. Each retry creates a new queue, so earlier activity cannot replay into it.
Other platforms retain their existing notify backend.

Bracket the unchanged content scans and index fingerprints with complete metadata observations
of all descendants, including ignored paths and Git administration. Each observation stores a
relative path mapped to device, inode, modification/change timestamps (including nanoseconds),
mode, owner, group, link count and size. Access time is excluded. A transient child creation or
removal changes its parent's stamp; content or metadata ABA changes the affected inode's stamp.
A changed observation invalidates the attempt even when the content scans agree.

Traverse with a stack of relative directory paths. Reopen each component from a clone of the
held root descriptor using `openat` with `O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC`, dropping the
previous lookup descriptor immediately. Enumerate `openat(parent, ".", ...)` with `Dir::from_fd`.
Open each child relative to the held parent with `O_EVTONLY | O_SYMLINK | O_CLOEXEC`, collect
metadata and immediately drop its descriptor. Queue directory paths without retaining handles.
Reopened directories must still match their queued metadata. No path-based recursion or symlink
target lookup is permitted. A replaced ancestor either fails the no-follow lookup or leaves a
held original directory being enumerated; it cannot redirect enumeration to an external tree.

The initial observation finishes before either content pass; the final observation begins after
both passes and the final index fingerprint. Thus every compared path's observation interval
contains the entire interval of content/index reads. Equal stamps for every path, including
parent directories that witness transient names, establish their shared unchanged interval.
The root watch covers rename, deletion and replacement of the held root even when its original
inode remains readable. All root mutation flags invalidate, except attribute-only notifications
whose root metadata still agrees; index read-atime updates do not invalidate. The 250 ms queue
wait uses one deadline, and an unsettled attribute storm fails closed. Observation, open,
metadata, enumeration, registration and queue errors fail closed too.

## Consequences

The three-attempt limit, 250 ms boundary wait, exact settled-content proof, two content scans,
index checks and all failure checks remain in force. Capture adds no source writes. Live
descriptor count is constant in both tree size and depth; metadata and pending paths require
memory proportional to the tree. True descriptor exhaustion still refuses capture and releases
partial resources, with no raised limit or partial-monitoring fallback. This replaces the
rejected per-path descriptor design, whose resource demand grew with ignored paths and whose
path-based recursion could follow a concurrently substituted directory symlink.

Native regressions cover a 64-descriptor child capturing 256 source files, 256 ignored
directories and an 80-level ignored tree; true handle exhaustion; directory replacement after
open and during component resolution; transient creation, metadata/content ABA, root replacement,
links, retries and read access. Native execution is required to establish watcher correctness;
command-sandbox passes alone are insufficient.
