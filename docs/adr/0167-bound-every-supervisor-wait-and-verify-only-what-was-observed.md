# ADR-0167: Bound every supervisor wait and verify only what was observed

**Status:** accepted (2026-10-19); decision 8 amended and the result superseded by
`af.perf-supervisor-result/3` in
[ADR-0168](0168-run-frozen-shards-with-whole-cli-oracles-and-only-a-per-sample-scratch.md).
The p2/p3a closeout repair of the
[continuous performance laboratory](../design/continuous-performance.md). Amends decisions 5,
6, 7 and 9 of
[ADR-0166](0166-build-frozen-plans-and-supervise-samples-with-one-authority-instrument.md):
the supervisor result becomes `af.perf-supervisor-result/2` and the capability report
`af.perf-capabilities/2`. The rest of ADR-0166 stands. Paired execution stays package p3b's.

**Amended 2026-10-28.** Decision 6's fill no longer syncs after every MiB. It still writes a
MiB at a time and counts every byte a write accepted. It syncs after every 64 MiB written
(`FILL_SYNC_BYTES`) and once more after a write the filesystem refused, before it reads the free
space. That sync's own error is the fill's stop: a delayed ENOSPC or EDQUOT verifies, EIO
refuses. Every other rule of decision 6 and every deadline of
[ADR-0173](0173-own-the-supervisor-a-test-runs-and-bound-admission-collection-scratch-and-cancellation-by-absolute-deadlines.md)
stand. The [verification note](../performance-local-executor-verification.md#scratch-enforcement-sync-batching-2026-10-28)
records why.

## Context

The sealed p3a output failed the mandatory kernel check, and independent root checks
reproduced five defects in it.

- **Kernel check.** One test of 2,083 failed:
  `task::optimization_configuration::tests::ordinary_command_tasks_consume_only_the_exact_admitted_cache_selection`
  panicked with `cache materialization failed`. The test kept only the error's `Display`, so the
  cause was lost. Run alone and in the full suite it passed. A stress run of 1,200 concurrent
  processes with the operator detail exposed failed 10 times, with
  `normalizing … metadata: File "/dev/fd/3": Bad file descriptor` or `No such file or
  directory`. On macOS the cache's ACL normalization addressed its open descriptor by the path
  `/dev/fd/<n>`. macOS resolves that path through devfs, and when many processes do the same,
  the lookup intermittently fails while the descriptor is open. p3a did not cause this. It is a
  flake in code that predates p3a, and nothing about cache identity or admission was involved.
- **Exit observation bypassed the deadline.** With `waitid` failing (injected EIO on Linux),
  `run_sample` fell back to a polling reap that read neither the deadline nor the cancellation.
  A `sleep 0.8` under a 100 ms timeout returned `exited 0` after 0.82 s, and a longer child
  could hold the supervisor indefinitely. The waits after a timeout kill or a cancellation were
  unbounded too.
- **A failed enumeration was clean.** An injected `opendir(/proc)` EIO produced
  `clean`, "verified by process_group_scan", with `remaining_after_cleanup: null`. The
  enumeration also took an iterator error, or a `/proc/<pid>/stat` that could not be read or
  parsed, to mean an exited process. The Python contract accepted the result.
- **The namespace guard lived only in the example.** The public `run_sample` accepted
  `Containment::PidNamespace` anywhere and called `kill(-1, …)` even after a normal exit. Outside
  a namespace's init, that reaches every process of the user.
- **The escaped-child test raced.** Its fixture backgrounded `setsid` and exited at once, so
  cleanup could kill the child before it left the group. A Linux run failed 21/22 that way.
- **I/O errors verified scratch capacity.** `fill` stopped at any error and took the errno as
  the filesystem's refusal. An injected EIO or EACCES on the first write reported enforcement
  verified with zero bytes written.
- **The integration test needed an undeclared build.** `cargo test --test perf_supervisor` from
  a clean target found no `examples/perf-supervisor`, because only a whole-workspace build
  builds examples.

## Considered options

1. **Retry the `/dev/fd` ACL calls while the descriptor is valid.** Rejected: it would assume
   that a racing devfs lookup can only fail, never resolve to another descriptor, and nothing
   here could prove that. A descriptor-based ACL call needs `unsafe` FFI, which the workspace
   forbids.
2. **Name the private cache entry itself without following its last component, bound to the
   held descriptor by identity before and after.** Chosen. The cache tree is private and no
   worker has started, which is the trust the function's other path-based steps already rest
   on.
3. **Keep the polling reap and add a deadline check.** Rejected on its own: an exit seen only by
   reaping ends the process group's reservation, so its interval and its cleanup must be
   reported as unestablished, not merely bounded.
4. **Report a lost exit observation as a launch failure.** Rejected: the child was launched, and
   the driver does not stop a shard for a launch failure.
5. **Keep the result and capability versions and change only the validators.** Rejected:
   `unobserved`, `status_unknown` and the fill evidence change the shapes, so ADR-0145 requires
   new versions.
6. **Make `perf-supervisor` a binary target so cargo exports its path.** Rejected: the
   laboratory builds, hashes and adapts it as an example, and that identity stays.

## Decision

1. **The namespace precondition is the shared leaf's.** `review_process::sample` exports
   `namespace_init()` (Linux and PID 1) and `NOT_NAMESPACE_INIT`. `run_sample` refuses
   `pid_namespace` containment anywhere else before anything is spawned, and the broadcast
   helper itself refuses `kill(-1)` outside a namespace's init (defence in depth). The example
   checks the same guard before it touches the scratch.
2. **Every wait is bounded.** One watch carries the child's exit. If the unreaped observation
   fails, the child is polled without blocking inside the same deadline and cancellation loop.
   If polling fails too, only the deadline or the cancellation ends the wait. A timeout kill
   waits at most one cleanup grace. A cancellation waits one grace for the forwarded signal,
   then sends SIGKILL and waits one more. An exit that is never seen is never waited for, so
   no reap can block. A process group is not signalled after a polled reap, because its id can
   be reused once the leader is gone; a namespace is always signalled.
3. **What was not observed is not claimed.** `child_wall_ns` exists only for an exit observed
   without reaping. An exit reaped by polling or never observed (`unobserved`), or one whose
   reap failed (`status_unknown`), is `cleanup_uncertain`, and the first reason quotes the
   original error. Rusage is unavailable for a child that was never reaped.
4. **Enumeration fails closed.** A process that vanished between the listing and its read
   (`ENOENT`, `ESRCH`) is gone. Any other failure to list `/proc` or to read or parse an entry is
   an error. At the exit or after cleanup, that error makes the lifecycle `cleanup_uncertain`
   with the original error first and the counts `null`. A failed kill is a reason too, except
   `ESRCH`, and except the `EPERM` macOS answers for a process group in which it found no member
   to signal, which is the group of an exited leader. A macOS group is never enumerated, so its
   lifecycle claims at most `no_observed_descendants` either way.
5. **`af.perf-supervisor-result/2`.** It adds the termination `status_unknown` and the exit
   observation `unobserved`. The reader checks every lifecycle claim against its counts:
   - Clean and unexpected descendants need zero remaining processes, no held pipe and
     descendants counted at an exit observed unreaped, and a clean lifecycle has none of them.
   - A held pipe, a remainder or an uncounted scan is `cleanup_uncertain`, and it states its
     reason.
   - Pipes alone count nothing.
   - A launch failure leaves nothing.
   - A scratch is cleared only after a verified namespace.

   `/1` is refused. The driver stops the shard for an exit not observed unreaped and for an
   unknown status, as it does for an uncertain cleanup.
6. **Scratch enforcement needs the filesystem's own refusal.** The fill writes and syncs a MiB
   at a time and counts every byte a write accepted, even when its sync then fails. It stops at
   the first error, a zero-byte write, `FILL_SECONDS_MAX` (600 s) or a byte past the limit, and
   it never retries. It records the stop, the errno and the call, the free space `statvfs`
   reported then (or why it could not), and any error closing or removing its file. Only
   ENOSPC or EDQUOT verifies enforcement, and only after progress, within the limit, with the
   filesystem then reporting less than two MiB free, and with the file removed. EIO, EACCES and
   every other error, no progress, the time bound, a filesystem that still reports space, and a
   failed cleanup each refuse, with the original error. `af.perf-capabilities/2` records this
   as the scratch's `fill` evidence, and its reader enforces the same rule; `/1` is refused.
7. **Fixtures prove their path.** The escaped child writes its pid from inside its new session
   while it holds the child's pipes, and the child exits only after that handshake. The test
   checks that the escapee leads its own session, kills it and waits until it is gone.
8. **The integration test builds what it tests.** `tests/perf_supervisor.rs` asks the cargo that
   runs it to build the example into its own target and profile, with the workspace's feature
   resolution. After a whole-workspace build that is a fingerprint check, never a second,
   different binary. A test binary started without cargo uses the example beside it as built.
9. **Cache ACLs are normalized without `/dev/fd`.** The macOS calls name the private entry with
   exacl's no-follow option and are bound to the held no-follow descriptor by device and inode
   before and after. A Task's cache materialization failure also writes its operator detail to
   stderr, as the check and build caches already do, while the durable error keeps only its
   `Display`.

## Consequences

- No supervisor wait outlasts the deadline plus two cleanup graces, and no observation,
  enumeration or kill failure can come back clean. Deterministic fault injection through the
  leaf's operating-system seam covers a failed `waitid` with a deadline, with a cancellation and
  with a normal exit, a failed `waitid` and `waitpid` together, and a failed enumeration. A
  child that ignores the deadline now ends at it. Three namespace tests (a new-session escape,
  a failed enumeration and a failed `waitid` under a deadline) are ignored by default and
  refuse to start unless the test binary is PID 1 of a disposable PID namespace.
- Validated on the development host (macOS, Rust 1.88.0):
  - All 16 host sample tests pass. All 9 `perf_supervisor` tests pass after the example binary
    was deleted: `cargo test --test perf_supervisor` rebuilt it through the nested build.
  - The new cross-process cache test fails on the old `/dev/fd` code
    (`File "/dev/fd/4": No such file or directory`) and passes on the fix.
  - The stress run of the failing Task test went from 10 failures in 1,200 to none.
  - The full workspace nextest `ci` run passes 2,091 tests, with 14 skipped: the 10 from before
    and the four new ignored ones. One earlier full run with these changes failed
    `campaign_loop::an_incomplete_run_does_not_burn_a_round` under load, with an event-store
    conflict. That test passed five isolated reruns and the final full run, and this change
    does not touch it.
  - The laboratory's 223 Python tests pass on Python 3.15 and 3.9.6.
- Validated on Linux arm64 (Docker Desktop, 6.10.14-linuxkit) with static musl test binaries, as
  uid 1000 without network:
  - All 29 `review-process` library tests and the 9 `perf_supervisor` tests pass under
    `--init`. The three namespace tests pass with the test binary as the container's PID 1,
    with no capabilities and 64 PIDs.
  - The supervisor ran as native PID 1 for four samples: a normal exit (clean), a `setsid`
    escape (one unexpected descendant killed, none remaining), a timeout (clean) and a 5-MB flood
    (counted past the caps). Each reply validated as `/2`. This was a functional check with
    64 PIDs and no scratch, not the declared profile or a measurement.
  - The existing `lib.rs` cancellation test failed in its `stdin` mode in that Debian `dash`
    container ("descendant must be live at cancellation: stdin"). It exercises the shared
    cancellable capture, which this change does not touch, and is recorded, not altered.
- No verified disk-bounded scratch exists on this host, so Linux container capability stays
  unavailable. A Linux host needs a dedicated block filesystem of at most 2 GiB, mounted at the
  configured path and verified with `cli.py capabilities --scratch PATH
  --verify-scratch-enforcement`, before p3b can run contained samples.
- The original failed receipts of the predecessor Task are AF's and are kept unchanged.
