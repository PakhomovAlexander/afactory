# ADR-0206: Contain telemetry experiments and collect only results verified under their authority

**Status:** proposed (2026-10-24). Amends
[ADR-0205](0205-integrate-telemetry-into-the-performance-lab-as-diagnostics-and-advisory-experiments.md)
for its experiments and their collection.
The consolidated integration includes published main and
[ADR-0208](0208-bind-calibration-dispatch-to-the-checked-main-source-and-separate-group-arguments.md).
The old cohort bytes and pinned collector remain frozen; its changed producer requires a new
cohort after the integrated repair is committed. Renumbered from unmerged ADR-0200 after
published main's ADR-0204.

## Context

ADR-0205 measured telemetry's own cost by running a pull request's candidate binary directly on
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
   itself.
   - **Paired scripts.** The script's own `summarize` must give back the whole document.
   - **Read latency.** Each latency must be the median of its samples, each bound the script's,
     and `within_bound` and `passed` what they give. It is judged only on the full corpus.
4. **Verification needs the exact authority.** A result is verified only under a checkout whose
   authority digest, file count and registry hash are the result's; anything else is refused,
   never kept as recorded. Only that authority's scripts are executed.
5. **The workflow uploads only admitted results.** Both sides are built in the pinned image,
   with a build record (`af.perf-telemetry-build/1`). Nothing is planned without the image.
   `telemetry-admit` verifies the result and scans it for credentials, and upload is
   conditioned on its `admitted` output. Otherwise the job fails and uploads nothing.
6. **A separate, bounded collection lane.** `cli.py telemetry-collect` (configuration
   `af.perf-telemetry-collector/1`, cycle record `af.perf-telemetry-collection/1`) collects
   only `perf-telemetry-<run>-<attempt>` artifacts of completed runs of
   `performance-telemetry.yml`.
   - **Collection.** It reuses the archive collector's credential confinement and its bounded
     transport and ZIP reading (`transport.read_zip`). It checks the declared SHA-256 and binds
     each result to its run, attempt, event and commit.
   - **Authority.** It requires the authority commit to be reachable from a trusted ref of a
     trusted mirror, and verifies the result under that commit's authority files read from Git
     objects.
   - **History root.** The root is locked, staged and bounded by bytes, results and retention.
     A systemd service and timer example lives in `scripts/performance/service/`; nothing is
     installed.
7. **The archive lane remains distinct.** The old cohort file is unchanged, and its archive
   collector remains pinned to that cohort's control. The current workflow includes published
   main and ADR-0208's repair and cannot produce evidence for that old authority. In current
   source, only the archive collector's bounded ZIP reading is shared through
   `transport.read_zip`; the two lanes never read each other's artifacts.

## Consequences

- **Contracts.** `af.perf-telemetry-result/1`, `af.perf-telemetry-history/1` and
  `af.perf-telemetry-lab/1`, new with ADR-0205 and never released, change shape in place: the
  result's execution, build and corpus facts, the history's bounds, mode and pruning, and the
  registry's containment. `af.perf-telemetry-build/1`, `af.perf-telemetry-collector/1` and
  `af.perf-telemetry-collection/1` are new.
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
