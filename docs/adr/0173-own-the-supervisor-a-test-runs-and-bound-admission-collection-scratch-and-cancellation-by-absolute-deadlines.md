# ADR-0173: Own the supervisor a test runs, and bound admission, collection, scratch and cancellation by absolute deadlines

**Status:** accepted (2026-10-20). Closes out package p4 of the
[continuous performance laboratory](../design/continuous-performance.md) against the sealed,
unaccepted output of
[ADR-0172](0172-admit-candidate-files-through-bounded-descriptors-and-report-archive-and-trend-runs-offline.md).
That output's mandatory gate failed, so it received no semantic review. Its decisions, and those
of
[ADR-0171](0171-cancel-samples-through-their-supervisor-carry-the-deadline-through-preflight-and-observe-history-order.md),
stand except where this ADR amends them:

- ADR-0171 decision 1: the cancellation's launch gate, and a container's one removal.
- ADR-0171 decision 3: one absolute deadline for the scratch check.
- ADR-0172 decision 1: incremental enumeration.
- ADR-0172 decision 7: the collector's absolute request deadline.

No persisted typed shape changes, so no contract version changes.

## Context

**The gate failure.** The P4 gate ended with 2,116 of 2,117 Rust tests passed. The failure was
`perf_supervisor::declared_json_fields_are_observed_over_output_beyond_the_cap`, which panicked
when it started the supervisor: `NotFound`. That happened before the supervisor ran, so it is no
evidence about the JSON observer. The cause, established here:

- The test starts `<target>/<profile>/examples/perf-supervisor`. Each test process first asks
  cargo to build that example (ADR-0167, ADR-0168). Nextest runs every test in its own process,
  so every process makes that build.
- A cargo build places a root unit's output there again on every run, even a fresh one. It
  skips that only when the path already is the very file of its `deps/` output. Otherwise it
  removes the old file and then links or copies the new one. On macOS it always copies,
  instead of hard-linking (rust-lang/cargo#10060), so every build removes and rewrites the file.
- Cargo's build lock serializes those builds, but not the other tests' spawns. One test's
  build removed the path another test was about to start.
- **Observed here.** After a workspace `cargo test --no-run`, the uplifted example and its `deps/`
  output were two files, each with one link. A fresh `cargo build --example perf-supervisor`
  replaced the uplifted file's inode.
- **Reproduced.** In four nextest runs of `perf_supervisor` alone with 16 threads, one failed
  with `cannot start the supervisor: [Errno 2] No such file or directory:
  …/examples/perf-supervisor`. An earlier set of three runs had 2 and 13 failures. The native
  full-stream checks that passed before ran a separately built, sealed supervisor, so they
  never met this path.

**Findings.** Root confirmed these against the sealed source:

- **p4-root-1, eager enumeration.** With 2 entries allowed, `admission._listing` listed and
  sorted all 8 names of a real directory before the bound refused it. Enumeration checked no
  deadline.
- **p4-root-2, a slow body passed the HTTPS deadline.** Over verified localhost TLS, an API
  `302` led to storage on another origin, which sent four body bytes 0.4 s apart. The
  collection's bound was 1 s, and `TransportError` came after 1.645 s. A read waits for
  trickling bytes under a per-read timeout, and checks between reads bound nothing.
- **lifecycle-bugs-1, a late capacity answer.** The fill's time bound was checked before each
  write, never after. With one second to fill, a write that took five seconds to return
  `ENOSPC` was recorded as a refusal after 5,000 ms, and `fill_refusal` accepted it.
- **lifecycle-correctness-2, a reset allowance.** `prepare_container` computed a relative fill
  allowance once. The scratch check then ran its metadata steps, and the fill started its own
  clock afterwards. With one second left, a two-second `mount_of` started the fill after the
  deadline, still with its whole second.
- **lifecycle-bugs-3, removal outlived the deadline.** `docker rm` and `docker ps` each had
  60 s. A failed launch removed the container in its cleanup callback and again in
  `run_contained`'s `finally`. That came to up to 120 s, or 240 s on failure: 236 s of
  controlled time against a 60 s caller deadline.
- **lifecycle-bugs-4, cancellation raced the launch.** `run_sample_on` checked
  `control.cancelled()` and only later called `posix_spawn`. A macOS interposer called the
  public `Control::cancel(15)` just before the real `posix_spawn` and saw `cancelled() ==
  Some(15)`, yet the child was spawned.

## Considered options

1. **Rerun the gate, retry the spawn or lengthen a wait.** Rejected. Nothing would explain or
   fix the removal, and the task forbids it.
2. **Build the example once per test binary, or skip the build.** Rejected. Nextest runs
   processes, not threads, and a stale example is the defect ADR-0167 closed.
3. **Run cargo's `deps/` output directly.** Rejected. Cargo names its hash nowhere stable.
4. **Each test process runs its own snapshot.** Chosen. The build and the copy happen under one
   exclusive lock that every process of the test takes, and tests start only the snapshot.
5. **A mutex across the spawn, which `cancel` waits for.** Rejected. A cancellation made on the
   spawning thread itself would deadlock, as in root's interposer, and the signal forwarder
   would block.
6. **One atomic state word, where the launch gate is the linearization point.** Chosen.
   Nothing waits, and every cancellation is ordered before the gate or after the spawn.
7. **Check the clock after each fill call only.** Not enough alone. A call that never returns
   is not bounded, so the fill's calls also run in a worker that the caller waits for only until
   the deadline.
8. **Shrink per-read socket timeouts.** Rejected. Reads that each return in time can still
   trickle past any total bound, as can header lines and a TLS handshake.

## Decision

1. **The supervisor a test runs is its own** (`crates/review-process/tests/perf_supervisor.rs`).
   - **Locked build.** Under an exclusive `flock` on `<profile>/perf-supervisor-test/lock`, the
     test builds the example exactly as before.
   - **The snapshot.** Still under the lock, it copies the example to
     `perf-supervisor-<FNV-1a>-<length>` beside the lock. A copy is renamed into place only
     after its bytes are verified, and a name that already holds exactly those bytes is reused.
     No build touches it, and one exists per distinct build.
   - **Unchanged.** Tests start only the snapshot. The fixtures, assertions, timeouts and the
     JSON observer are the same. A test binary started without cargo still runs the example
     beside it as built.
2. **The launch gate orders cancellation and spawn** (`review_process::sample::Control`).
   - **The state.** One atomic word holds the first signal recorded and a `LAUNCHING` flag.
   - **Before the gate.** A launch passes the gate only while no cancellation is admitted, so a
     cancellation admitted first means `posix_spawn` is never called. The earlier separate check
     is gone: the decision and the spawn are one step.
   - **Inside the gate.** A cancellation recorded while a launch holds the gate is admitted when
     the gate opens, once the child exists. `cancelled()` does not report it before then. The
     launched child is then cancelled through its containment like any later cancellation, by
     the forwarded signal, the kill after the grace and verification.
   - **Unchanged.** Nothing waits, so the signal forwarder never blocks. The leaf's shared
     process semantics, SIGTERM forwarding and setup exclusion are unchanged.
3. **One absolute deadline bounds the scratch check** (`perflab.capability`).
   - **Fixed once.** The shard fixes it before the first step, with the new `Budget.until`: the
     earlier of the fill's 600 s bound and its run's remaining time less the terminal reserve.
     It is an instant of the shard's own clock.
   - **Each step.** Resolving the path, finding the mount, `statvfs`, the listing and the fill
     each start only before it. A step it left no time for refuses the check with a deadline
     reason, and the shard reports that as `run_deadline`.
   - **The cleanup allowance.** No write starts in the last 30 s, which are kept for the free
     space at the stop and the file's close and removal. When that much is not left, no fill
     file is created.
   - **Late answers.** A write or sync that returns after its bound stops the fill as
     `deadline`, whatever it returned. So does any answer received after the deadline. A late
     `ENOSPC` never verifies a bound.
   - **Blocked calls.** The fill's calls run in a worker, and the caller waits for it only until
     the deadline. A worker still blocked then is abandoned as `deadline`, and its cleanup is
     unconfirmed. Its file is kept and named, and the worker makes no further call except
     closing its descriptor. The next check finds the file and refuses the filesystem as not
     empty.
   - **Unchanged.** The real 2-GiB disk contract and every enforcement and error check stay.
     Nothing falls back to tmpfs or retries.
4. **A container has one removal owner** (`perflab.supervise`).
   - **One removal.** `run_contained` removes its container once, whatever ended the launch.
     `launch` no longer removes anything.
   - **Its allowance.** `docker rm` and the confirming `docker ps` share 60 s, which also end by
     the run's deadline. Neither starts once it is spent, each is bounded by what remains, and a
     removal not confirmed within it is uncertain.
   - **Kept free.** A contained launch keeps that allowance free before the deadline, beside its
     cancellation reserve and start. An admitted slot already leaves at least the 90 s container
     margin and the 30 s terminal reserve, so no admitted slot is refused for it.
   - **Uncertainty.** Only a confirmed removal makes a failed contained launch certain. A
     launch that never started `docker run` removes nothing.
5. **Enumeration is bounded while it reads** (`perflab.admission`).
   - **One name at a time.** A directory is read through `scandir` on a fresh description of
     its held descriptor.
   - **The bound.** At most the entry bound's remaining room is ever held or sorted. A listing is
     refused at the first name beyond it, and the deadline is checked before each name.
   - **Unchanged.** Descriptor confinement, the sorted order, `O_NOFOLLOW` and refusal before
     any mode change stay.
6. **Every HTTPS exchange has one absolute deadline** (`perflab.transport`).
   - **The bound.** A request's bound is the earlier of its 60 s and the collection's remaining
     time.
   - **What it covers.** The name lookup, each address's connection, the TLS handshake, the
     request, the headers and every body byte.
   - **How.** The exchange runs in a worker, and each blocking call is given only what is left.
     At the deadline the caller shuts the socket down and gives the worker 1 s to end, then
     refuses. An abandoned exchange opens no connection and sends no request.
   - **Admission.** A response that completes after its deadline, or after the collection's, is
     refused. Redirect hops share the collection's deadline.
   - **Unchanged.** Certificate and host-name verification stay on, through the system trust or
     a given `SSLContext`, and the token still goes only to the API origin. Diagnostics show
     bounds to the millisecond, such as "0.75-second", instead of truncating them to whole
     seconds.

## Consequences

- **No contract changes.** No persisted shape changed. Every fill a capabilities document can
  now hold, an abandoned one included, validates as `af.perf-capabilities/2`, and a late
  answer is recorded as `stopped: deadline`. The 15-second production lease, the 8-MiB envelope
  cap, every sample timeout, cleanup grace, log cap, the 2-GiB scratch, the 600 s fill bound,
  the run deadlines and the terminal reserve are unchanged.
- **Rust tests.**
  - A deterministic boundary test copies the actual supervisor and snapshots it, then performs
    cargo's two steps in order. After the removal, starting the uplifted path fails with
    `NotFound`, exactly as the gate did, while the snapshot runs a real sample. After the copy,
    the snapshot is unmoved. Other bytes make another snapshot.
  - A second test rebuilds in the same process and finds the running snapshot untouched.
  - On the final source, five nextest runs of all 21 `perf_supervisor` tests, 16 threads
    each, passed with no `NotFound`. That is supporting evidence, not the proof; the
    deterministic test above is.
  - **Launch gate unit tests.** A cancellation before the gate reaches no spawn. A
    cancellation made inside the gate, on the launching thread as root's interposer did or by
    another thread, is not admitted before the spawn returns. A barrier race over 2,000 rounds
    never meets a spawn that ran with a cancellation admitted.
  - **The real spawn.** A cancellation completed at the spawn boundary cancels the launched
    child as `cancelled`.
- **Python tests.**
  - **The fill.** A five-second late `ENOSPC`, a late sync and a late `statvfs` are the
    deadline's. A write still blocked at the deadline is abandoned, its file kept, and the
    worker writes nothing more. A slow `mount_of` or listing starts no fill, both in
    `capability` and through the actual shard constructor, which reports `run_deadline`.
  - **The container.** A failed launch removes its container once. Against a 90 s deadline,
    controlled time shows `rm` with 60 s, then `ps` with the 1 s left. No removal starts with
    nothing left.
  - **Enumeration.** An 8-file directory under a 2-entry bound is refused after 3 names, nested
    rooms are counted, and the deadline stops a listing between names.
  - **HTTPS.** Real localhost TLS uses a certificate made by `openssl` for the test; the test
    skips with that reason when no `openssl` exists. A body or headers trickled 0.4 s apart are
    refused at a 1 s or 0.75 s deadline. Redirect hops that each fit alone are refused together
    at the collection's deadline, and the token never reaches storage. Untrusted certificates
    and mismatched host names still fail verification.
- **Not evidence.** This is not calibrated performance, a hosted or Linux container run, a real
  2-GiB fill, an unattended collection or a production signal trace. The cancellation gate is
  shown by deterministic boundary injection and synchronized tests. The p4 report, archive and
  history modules still need their full semantic review, which this closeout does not replace.
