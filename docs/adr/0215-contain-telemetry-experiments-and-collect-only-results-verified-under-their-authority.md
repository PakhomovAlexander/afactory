# ADR-0215: Contain telemetry experiments and collect only results verified under their authority

**Status:** proposed (2026-10-24). Amends
[ADR-0214](0214-integrate-telemetry-into-the-performance-lab-as-diagnostics-and-advisory-experiments.md)
for its experiments and their collection.
The consolidated integration includes published main and
[ADR-0217](0217-bind-calibration-dispatch-to-the-checked-main-source-and-separate-group-arguments.md).
The old cohort bytes and pinned collector remain frozen; its changed producer requires a new
cohort after the integrated repair is committed. Renumbered from unmerged ADR-0200 after
published main's ADR-0204.

## Context

ADR-0214 measured telemetry's own cost by running a pull request's candidate binary directly on
the measuring runner. The trusted control's scripts, the result directory and the other side's
binary sat beside it, and a new Python process supervisor had deadline, pipe and cleanup
semantics of its own. Review of that draft found these problems:

- **Containment.** The candidate could write anywhere the runner's user could.
- **Teardown.** Teardown killed a process group without proving anything ended.
- **Work bytes.** Work bytes were counted only after a script exited, and the corpus not at
  all.
- **Authority.** `verify` skipped judgment whenever a result named another authority, and
  history kept such a result as verified.
- **Read latency.** Its verdict trusted the document's own `within_bound` and `passed` and a
  corpus name.
- **Plan.** A document was compared with the other side's but never with its plan.
- **Upload.** The workflow uploaded results unconditionally.
- **Collection.** Collection was manual.

The shared `perf-supervisor` (ADR-0026, ADR-0166) cannot simply be reused for a bench script.
Its request bounds a child at the verdict sample's 120 seconds, and the scripts run for
minutes. Raising that bound changes a protected contract of the verdict path. The frozen hosted
cohort's collector stays pinned to its own control and report generations, and ADR-0145 forbids
a reader of earlier versions.

## Considered options

- **Raise the supervisor's bound or add a long request kind.** Rejected: it changes the
  verdict path's protected request contract, its fixtures and the harness digest. A sample would
  then be accepted with a bound the suite never needs.
- **Split a script's pairs into runs under 120 seconds.** Rejected: it changes the scripts'
  protocols (one seeded order over all pairs, one bootstrap) that their own judgment depends on.
- **Wrap each `af` command in a container.** Rejected: the container start would sit inside
  every measured wall time.
- **Keep the candidate on the runner and rely on offline re-verification.** Rejected: whatever
  ran beside the controller could change the controller, the other side or the evidence.

## Decision

1. **Every execution goes through one executor** (`perflab/telemetryexec.py`): each side's
   `--version` query, the corpus generator and every script run, within the run's one deadline
   and its 90-second teardown reserve.
   - **Containment.** `linux_container` runs each in its own container of the pinned sample
     image: network none, read-only root, no capability, no new privileges, `docker-init` as
     PID 1, the registry's PID, memory (no swap) and CPU limits, `RLIMIT_FSIZE`, this user.
   - **Binds.** Read-only, at their own paths: only the experiment's declared inputs and that
     side's binary. Writable: only the execution's own new directory under the verified
     disk-bounded scratch filesystem's `telemetry/` entry, checked with `prepare_scratch` and
     `scratch_problems`.
   - **Teardown.** The PID namespace ends every descendant, and `supervise.remove_container`
     removes the container once.
   - **Trusted local.** `trusted_local` runs on the host for source the operator trusts and
     says what it cannot confine. A pull request is measured only contained. Its leader stays
     unreaped until all group signals and observations end, reserving the group's identity.
     Like `review_process::sample`, Linux observes exit with `waitid(WNOWAIT)` and enumerates
     the group's executing members; macOS uses kqueue `NOTE_EXIT` and its zombie-excluding
     group signal probe. Only after cleanup is the leader reaped. These OS observations stay
     in the existing file-backed executor: using the verdict helper would require changing
     its protected duration/protocol or splitting the experiment. No new native supervisor
     protocol or change to the verdict sample's 120-second bound is introduced.
2. **Bounded while written.** Stdout and stderr go to controller-owned capture files the
   execution cannot reach. One loop checks the deadline and, about once a second, everything
   the execution may write against the profile's work bytes, and stops it there. Both binaries,
   the authority, the master corpus and the result directory are checked after every execution.
   - **Problems.** A change is a problem, which stops the run.
   - **Uncertain cleanup.** An unconfirmed removal, a changed scratch filesystem or a group
     still alive stops the run and keeps its work for the operator.
     On Linux a complete, bounded `/proc` scan must establish that no group member can
     execute. Terminal states `Z`, `X` and `x` cannot execute or retain open file descriptors;
     an ancestor subreaper may still retain them. A terminal thread-group leader alone does
     not establish this for its other threads: the same bounded `/proc/<pid>/stat` read must
     also report exactly one thread. Any additional thread counts as potentially executing.
     A missing reserved leader, an enumeration, read, parse, ownership, signal or
     exit-observation error, or an exceeded scan bound makes
     cleanup uncertain even if a later observation succeeds. Only entries that vanished
     during the scan may be absent. Group killing repeats within the unchanged cleanup grace
     while the leader reserves its identity. A live descendant observed at script exit still
     fails the execution, even when its cleanup is confirmed; a deadline stays a timeout.
   - **Status.** Neither is ever `complete`.
3. **Admission recomputes everything from the plan.** A document must name its side's binary
   version and the planned pairs, scale, seed, fixtures, cases and repeats. Read latency must
   name the corpus this result generated, by runs, seed, name and file facts the controller read
   itself. Every indexed count is derived independently from the trusted generator layout: executions,
   spans, events, distinct/shared blobs, occurrences and retained bytes. Protocol v2 also records
   `content_indexed`; content indexing must be enabled and every generated text blob indexed.
   - **Paired scripts.** The script's own `summarize` must give back the whole document.
   - **Read latency.** Each latency must be the median of its samples, each bound the script's,
     and `within_bound` and `passed` what they give. It is judged only on the full corpus.
4. **Verification needs the exact authority.** A result is verified only under a checkout whose
   authority digest, file count and registry hash are the result's; anything else is refused,
   never kept as recorded. Its closure includes every `perflab` module, all schemas and performance
   fixtures, the CLI, workflow, bench inputs and build/toolchain manifests. A retained checkout
   must also match the installed trusted verifier's complete closure before any authority script
   is loaded. Another installed verifier is refused, even if the retained tree matches the result;
   retained code is data, never an automatically selected executable verifier. CLI startup
   refuses cached bytecode before any laboratory import, and authority admission refuses
   cached executable artifacts. Bench modules compile their admitted source bytes directly;
   `-B` alone would still permit Python to read a stale or foreign cache.
5. **The workflow uploads only admitted results.** Both sides are built in the pinned image,
   with a build record (`af.perf-telemetry-build/1`). Nothing is planned without the image.
   `telemetry-admit` verifies the result and scans it for credentials, and upload is
   conditioned on its `admitted` output. Otherwise the job fails and uploads nothing.
6. **A separate, bounded collection lane.** `cli.py telemetry-collect` (configuration
   `af.perf-telemetry-collector/1`, cycle record `af.perf-telemetry-collection/2`) collects
   only `perf-telemetry-<run>-<attempt>` artifacts of completed runs of
   `performance-telemetry.yml`.
   - **Collection.** It reuses the archive collector's credential confinement and its bounded
     transport and ZIP reading (`transport.read_zip`). It checks the declared SHA-256 and binds
     each result to its run, attempt, event and commit.
   - **Authority.** It requires the authority commit to be reachable from a trusted ref of a
     trusted mirror, and verifies the result under that commit's authority files read from Git
     objects.
   - **History root.** The root is locked, staged and bounded by bytes, results and retention.
     Trusted authority trees are retained separately under `authorities/<digest>/`, included in
     the byte bound, reused only by exact identity and swept after their last result is pruned.
     History re-verifies every result against its retained tree and the matching installed
     verifier; missing, altered or incompatible authority never becomes a verified trend.
     A systemd service and timer example lives in `scripts/performance/service/`; nothing is
     installed.
7. **One absolute collection deadline.** The transport's original monotonic deadline also
   bounds Git reachability/object subprocesses, authority copies, verification, import, retention
   and history/report derivation. Git receives the existing `Budget`; a Unix main-thread
   `Budget.enforce` interval timer interrupts synchronous work, with an interruption type that
   per-artifact refusals cannot swallow. No credential-bearing worker or artifact-code loader
   is introduced. An existing interval timer or unsupported invocation is refused.
   - Five seconds of the same deadline are reserved for cleanup and atomic terminal recording.
     An expired operation leaves unaccepted staging for the bounded locked sweep, rather than
     entering an unbounded `finally` deletion. Lock acquisition failure leaves the owner alone;
     failure after acquisition closes the descriptor, including failures during entry cleanup.
     SIGALRM is deferred only across the nonblocking lock-file open and descriptor publication,
     and across close/clearing ownership. A temporary Python handler latches expiry, then
     delivers it immediately after transfer, including nested exceptional unwinds. Thread-local
     `pthread_sigmask` alone cannot do this: an unmasked cancelled resolver can receive the
     process alarm and schedule Python's handler on the main thread. The handler is restored;
     the absolute timer and caller's signal mask never change. No operation-wide deferral is
     allowed; flock is nonblocking, with the descriptor already owned before it runs.
     Any exceptional exit closes that owned descriptor, never unlocking another owner's lock.
   - HTTP cancellation includes every exceptional caller exit, including deadline interruption
     while waiting. It revokes the exchange and shuts down/closes its socket before terminal
     accounting. Socket-to-TLS descriptor transfer is synchronized with cancellation, without
     handshake or network I/O under that lock. Cleanup waits at most one second within the
     request deadline and the enclosing timer. A blocked system resolver cannot be killed:
     its cancelled daemon thread may remain alive, with this uncertainty recorded, and cannot
     connect, do TLS or send an authenticated request when lookup returns. The worker's own
     cleanup receipt, never an interrupted thread join's apparent state, establishes completion.
     Unconfirmed cleanup prefixes the bounded exception message, preserving its original type
     on Python 3.9+ without exception notes. The marker also survives Collector's redaction of
     OS/HTTP failures. Outer cycle detail, per-run/per-artifact refusals and shared archive
     refusals retain it, including an ordinary request timeout before the collection deadline.
     Confirmed failures carry no cleanup warning. ADR-0197 changes only CI selection, not this
     runtime contract.
   - Imports publish only a complete, verified staging directory. Expiry immediately around
     its rename is reconciled under the lock. A fully verified result accepted before history
     expiry remains kept, while cycle accounting stays unknown and failed.
   - Collection v2 uses null result/byte counts when history construction did not finish,
     and a null terminal time if terminal recording itself did not complete. An
     atomic incomplete record precedes work and terminal cleanup, so interrupted final writes
     cannot leave a success record for the current cycle. The reserved timer also bounds
     cleanup/rendering/status; the service timeout is a separate outer backstop.
8. **The archive lane remains distinct.** The old cohort file is unchanged, and its archive
   collector remains pinned to that cohort's control. The current workflow includes published
   main and ADR-0217's repair and cannot produce evidence for that old authority. In current
   source, only the archive collector's bounded ZIP reading is shared through
   `transport.read_zip`; the two lanes never read each other's artifacts.

## Consequences

- **Contracts.** Read-latency protocol/schema v2 adds `content_indexed`; registry v2 names
  that protocol. History v2 records each retained `authority_path`; collection v2 permits
  explicitly unknown accounting. Authority identity uses domain v2 and the complete closure.
  Writers, readers, schemas and fixtures change together; earlier shapes and incomplete
  historical identities are refused without fallback (ADR-0145). Result v1's typed shape
  remains unchanged; its old authority identities cannot verify under the new closure.
- **Limits.** The contained limits are those the container was started with; they are not read
  back from its cgroup. Within one execution a candidate shares the script's user and can still
  falsify that run's own document, never the other side's, the controller's or the history's.
- **Hosts.** Contained experiments need a Linux host with a cgroup v2 container runtime, the
  pinned image and a disk-bounded scratch filesystem of the profile's work bytes. The release
  profile's 12 GiB may not fit a standard hosted runner, which then refuses before measuring.
- **Implementation evidence only.** At this draft's 2026-10-24 implementation checkpoint, no
  hosted execution or collection-host deployment was demonstrated. Later operational status
  must come from the operator's dated deployment and run records, not this source document.

## Retaining ancestor subreaper regression

The native check driver is a Linux child subreaper (`review-process/reaper.rs`). A telemetry
script's orphaned descendants can become that driver's children, which it reaps when its check
ends. The predecessor executor reaped its own leader early, then used `killpg(pgid, 0)` to
decide whether cleanup ended. That probe reports even a zombie-only group as present on Linux.
The executor could neither reap the ancestor's children nor make another kill remove a zombie,
so both a leftover child and a deadline incorrectly ended as uncertain after ten seconds.
Releasing the leader before the last group signal also lost the group's identity reservation.

An isolated helper subprocess now becomes the subreaper, starts the real executor in a nested
process, and retains adopted children until that executor returns. Bounded fixture-only
PID/state/parent/group/session observations reproduce both predecessor failures and show the
adopted zombie with the helper still alive. The repaired executor confirms no executing group
member remains while preserving the distinct leftover and deadline failures. A negative fixture
retains a genuinely live survivor and remains uncertain, including a zombie thread-group leader
whose worker thread can still execute. None of these implementation checks establishes independent
acceptance, hosted performance, a valid cohort or deployment.

## Deployment repair (2026-10-25)

The installed cohort dispatcher now validates the bounded whole installed tree before importing
`perflab`: no bytecode, unlisted files/directories, native modules, links, duplicate paths or
previously loaded package authority. A package-specific loader compiles the already verified
source buffers, including later lazy imports, without re-opening source or reading caches.
`-B` prevents writes but cannot prevent cache reads; a
subprocess regression retains a timestamp/size-valid poisoned cache with all source hashes
unchanged and proves refusal before payload or credential work.

Telemetry planning now retains `af.perf-telemetry-binding/1` independently of result claims.
The trusted workflow retains the original schedule and emits one original-event/frozen-source
binding per measuring attempt, including failed-jobs reruns. Collection requires its immutable
artifact digest and exact installed producer workflow, original run/event/attempt/head, and all
three planned source identities. PRs measure their original merge, whose ordered parents bind
original baseline and immutable run branch head. Mutable linked PR metadata is never used.
The checked receipt is retained under the history lock and byte cap, and replayed as data.
Existing result/history shapes do not change; the new binding has its own closed schema and
fixtures. Older missing bindings fail closed.

Hosted compilation reuses `BuildImage` and `ContainerToolchain` with the runner UID/GID,
read-only source and owned writable Cargo/Rustup/target directories. Tool discovery happens
without candidate mounts; compilation invokes the verified compiler directly. No capabilities,
credentials or Docker socket are granted to the candidate. A bounded CI smoke compiles a
minimal fixture through that exact command on a runner-owned 0755 checkout. Docker execution
and eventual hosted measurements remain separate operational obligations.

Report publication uses two fixed names in marked owned staging under the history lock.
Interrupted writes and replacements remain bounded and are swept on entry within the existing
deadline; foreign entries and links are refused. Root accounting includes reports, staging,
bindings and retained foreign files. Publication reserves the peak coexistence of complete
reports and both replacements, with exact/insufficient-capacity and interruption regressions.
The old cohort bytes, calibration policies, release metadata and Rust behavior are unchanged.

## Build and collection publication boundary repair (2026-10-25)

Dependency preparation and compilation are separate invocations of the verified image's
Cargo. Only `cargo fetch --locked` has network access. Before it, Cargo configuration is
refused using the existing build authority rule (only an overridden `build.target-dir` is
allowed). There are no candidate hooks, wrappers, credential providers or host configuration
in this preparation. Compilation is `cargo build --release --locked --offline --bin af
--target-dir /state/target`, with network disabled and the existing non-root, read-only,
capability-free boundary. Tool discovery has no candidate mount. Real commit bytes are copied
into a minimal read-only Git repository, with no host configuration or credentials, so the
read-only source still embeds its real source identity.

`telemetry-contained-build --source DIR --source-sha FULL_SHA --output NEW_DIR --image DIGEST
--scratch-fs NEW_MOUNT` prepares an owned 8 GiB block filesystem through `hostprep`; an existing
fresh dedicated filesystem may be smaller but never larger. Enforcement is verified before
any build. HOME, TMPDIR, Cargo/Rustup homes, target and bounded controller logs all share this
aggregate ceiling, including unlinked open files. Only the candidate's subdirectory is mounted;
logs are separately owned, preallocated and capped, Docker logging is disabled and no shared
memory filesystem is writable. The 10 GiB host free-disk floor remains, including admission's
copy. One deadline (at most 4,800 seconds, 30 reserved for completion) covers preparation,
probes, fetch, compilation, admission and cleanup. A failure retains bounded scratch and says
so; unconfirmed container cleanup never permits publication or deletion of its backing image.
Successful owned filesystem teardown precedes publication.

Trusted tool metadata is written only in a controller directory never mounted writable.
`admission.open_confined` and bounded descriptor copying admit the binary, rejecting links at
every component, nonregular files, excessive logical size and mutation. The workflow copies
only that admitted binary. The executable validation smoke exercises a prefetched registry
dependency, real Git identity, a reachable TCP sentinel denied to build.rs, many small files,
unlinked open writes, metadata/binary/ancestor links, FIFO and oversized output on small real
Docker filesystems. A worker without Docker cannot establish these live results; hosted CI
must be observed before rollout.

Collection status and position use the marked report staging directory and the same locked
root capacity policy. Every replacement reserves old and staged allocations before writing;
initial ownership metadata also reserves capacity. Final status accounting resolves its own
serialized size and includes position, reports, incomplete reserve and owned leftovers. A
compact incomplete record stays allocated through success. At the next cycle it can replace
an old success by rename even at full capacity. Interrupted writes remain bounded owned
staging, swept on locked restart; foreign root temporaries are counted and retained, and
links or foreign staging are refused. Complete prior reports survive capacity refusal. These
are publication repairs within the existing typed formats, with no compatibility reader,
release change, cohort change or statistical change.
