# ADR-0165: Own probe scratch in the driver and take lease expiry only from the Store

**Status:** accepted (2026-10-19); decisions 2 and 3 and the cleanup residual of its
consequences superseded by
[ADR-0166](0166-build-frozen-plans-and-supervise-samples-with-one-authority-instrument.md).
Supersedes decisions 3 and 6 of
[ADR-0164](0164-judge-only-declared-probe-requests-against-a-reference-checked-golden.md) and
the cleanup in decisions 1 and 6 of
[ADR-0163](0163-measure-native-workloads-through-a-public-api-probe-and-independent-golden-oracles.md),
as the second p2 repair of the [continuous performance laboratory](../design/continuous-performance.md)
design. The rest of both stands; process-group and Linux namespace containment stay p3a's.

## Context

ADR-0164 had the probe create `<scratch>/probe-work`, record its device and inode, and at the
end remove it with `remove_dir_all` while the entry at that path still had that identity.
Review showed both checks were separate path operations. Another process could rename the new
directory and put its own there between `create_dir` and the identity read, so the probe
recorded the foreign directory as its own. It could also swap the path between the identity
check and `remove_dir_all`, which then removed the foreign directory recursively. The standard
library has no directory-handle removal. A handle-based removal in the probe would need raw
`libc` declarations or a new dependency, and the probe's helper modules must stay an
overlayable closure over the crate's existing dependencies. The driver also removed its scratch
with `shutil.rmtree(..., ignore_errors=True)`, so a failed cleanup counted as success.

Two lease defects remained. `classify` reported `lease_expired` whenever `task_lease_state`
returned an error, so a SQLite read error, a malformed transition or a clock error looked like
expiry. The state persistence fixture refreshed its lease and only then read the run ID and log
length, so a slow Store read after the refresh used up lease time before the region.

## Considered options

1. **Check identity again just before `remove_dir_all`.** Rejected: any check followed by a
   removal by path leaves the same window.
2. **Use `openat`/`unlinkat` in the probe.** Rejected: there is no safe std API for it, and raw
   `libc` declarations or a new dependency would break the overlayable closure.
3. **Rely on random names alone.** Rejected: a random name makes a collision unlikely but proves
   nothing about who owns the directory.
4. **The probe removes nothing; the trusted driver owns a private namespace and removes it
   through a handle.** Chosen.
5. **Retry a failed renewal or treat every lease-state error as expiry.** Rejected: a retry hides
   the side's failure, and an error that is not expiry is a different failure.

## Decision

1. **The probe removes nothing.** It still creates `probe-work` with `create_dir` in the empty,
   canonical scratch it is given. If that fails, the sample fails as `fixture_failed` before
   anything runs. The probe writes only below `probe-work` and never removes any path. Its
   caller owns the scratch and everything in it. A passed sample reports no cleanup it did not
   do. The request and message shapes are unchanged, so no version changes.
2. **The driver's private namespace.** `perflab.oracle.Namespace` checks that the `--scratch`
   parent is owned by this user or root and is not writable by others unless sticky. It creates
   `perf-probe-<128 random bits>` there with mode 0700 relative to a handle on the parent,
   before any probe exists. It then opens the new entry with `O_NOFOLLOW | O_DIRECTORY` and
   requires the handle to be the empty 0700 directory of this user now at that name. Anything
   else, such as a symlink, a non-empty directory or another mode, is refused and left in
   place, and nothing launches. The probe gets only this namespace's path. The caller's parent
   and its other entries are never removed.
3. **Handle-anchored cleanup.** `Namespace.remove` clears the namespace through its handle and
   through handles opened relative to it. Symlinks are unlinked and never followed. Each
   subdirectory must be the directory just observed at its name. The namespace's own entry is
   then removed only while it still has the held identity, and only with `rmdir`, which removes
   only an empty directory. The removal is confirmed through the handle: a removed directory
   cannot take a new entry. A replaced, vanished or unremovable entry is left in place and
   reported. An unconfirmed cleanup fails the sample: a passed sample becomes
   `lifecycle_violation`, and a failed one keeps its own kind with the cleanup appended to its
   detail. It is also written to stderr and never ignored. Tests check every interleaving with
   a boundary hook: after `mkdir`, after the contents are cleared, and after the identity check.
4. **Lease expiry only from the Store.** After a lease-bound failure, `cases::lease_state` reads
   the Store once. `Live(until)` means the Store observed the lease held. `Lapsed` means the
   Store returned `StoreError::Conflict` starting "Task writer lease is expired or fenced",
   which it uses only for an expired or fenced lease. `Unreadable` covers every other error,
   including SQLite, decoding, malformed transitions and the clock. `classify` returns
   `lease_expired` only for `Lapsed`. Otherwise it returns the operation's own kind
   (`operation_failed`, or `fixture_failed` in setup). The original diagnostic always comes
   first; an unreadable state is appended to it, and nothing is retried.
5. **Refresh last.** State persistence setup reads the run ID and the built log's length (3)
   first. Then it refreshes the 15,000-ms production lease as its last successful step. The
   refresh appends exactly one event, which is counted into `prefix_events` (4) without another
   Store read. The golden entry is unchanged. A scheduler pause can still fall between the
   refresh and the timer. When it does, it is reported as `lease_expired`, never retried.

## Consequences

- No path that another process can replace is deleted recursively, by the probe or the driver.
  Only one residual remains, because POSIX has no removal by handle. A process of the same user
  that swaps an empty directory into the namespace's name between the identity check and the
  `rmdir` loses that empty directory. The confirmation detects this and fails the sample. Other
  users cannot do this, because of the parent check.
- Running the probe directly, as the Rust tests do, leaves `probe-work` for the caller to
  remove.
- A corrupt or unreadable Task log during a lease-bound sample is reported as the operation's
  or fixture's failure with both diagnostics, never as expiry.
- Product constants, the native case coverage, the golden document and every persisted shape
  are unchanged.
