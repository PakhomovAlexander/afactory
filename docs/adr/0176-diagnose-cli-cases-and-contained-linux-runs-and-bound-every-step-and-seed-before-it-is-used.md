# ADR-0176: Diagnose CLI cases and contained Linux runs, and bound every step and seed before it is used

**Status:** accepted (2026-10-20). Completes package p5 (traces, Linux profiles and diagnostic
counters) of the [continuous performance laboratory](../design/continuous-performance.md). The
decisions of [ADR-0174](0174-diagnose-in-separate-child-runs-that-record-only-the-bracketed-region-and-share-one-test-executable-boundary.md)
and [ADR-0175](0175-supervise-every-diagnostic-execution-admit-what-it-wrote-and-record-what-it-used.md)
stand, except that this ADR replaces decision 2's contained refusal and decision 8's CLI
omission of ADR-0175.

| Contract | Version | What changed |
| --- | --- | --- |
| Profiles | `af.perf-profiles/4` | replaces `/3`, which is refused |
| Diagnostics registry | `af.perf-diagnostics/1` | unchanged shape; two musl recipes added |

## Context

ADR-0175's sealed source failed the mandatory gate twice, and root checks of it found five gaps:

1. **Contained Linux diagnostics.** `--mode linux_container` always refused as
   `contained_diagnostics_not_implemented`.
2. **CLI cases.** Every CLI case was omitted as `cli_case_not_implemented`, and the musl
   shipping recipe had no diagnostic recipe.
3. **A parsing prefix passed.** A child wrote 65,546 bytes. The supervisor kept 65,536, one valid
   padded JSON line, and reported the stream truncated. `run_probe` checked launch, timeout,
   lateness, lifecycle and cancellation but not truncation, so the oracle passed the prefix.
4. **Per-file bounds only.** `RLIMIT_FSIZE` bounds each file. A tool wrote two 16-byte files
   and 3 bytes of stdout under a 32-byte cap with 16 bytes of evidence room, and `tool_output`
   admitted it. `Budget.record` already reported the 35 bytes honestly.
5. **A seed followed a symlink.** A seed writer left a symlink to an outside file beside a
   passing message. `prepared` ran `shutil.copytree`, which copied the outside bytes.

The gate failures:

- **A compile-time path.** `crates/review-process/tests/diagnostic_tools.rs` used
  `env!("CARGO_MANIFEST_DIR")` without the runtime `AF_WORKSPACE_ROOT` fallback that warm gate
  caches need (ADR-0131).
- **A hand-off that never returned.** In `tui::a_command_that_ends_at_once_still_shows_its_exit`,
  after repeated `--version` hand-offs the browser never came back on Enter.

The original real `perf script` fixture of ADR-0174 is still missing, and its test was removed.

## Considered options

1. **Fork container handling for diagnostics.** Rejected. A shard's container is
   `supervise.run_contained`, and its preflight is the shard's. A second copy would drift.
2. **Bind the evidence directory writable into the container.** Rejected. It is not on the
   verified bounded filesystem, so nothing would bound what a container writes there.
3. **Run the run's own work on the verified filesystem, binding paths at their own paths.**
   Chosen. Seeds, each execution's private state and staged outputs live in one directory of
   that filesystem, so its bound is the aggregate bound. Paths need no translation, and every
   staged output is admitted into the evidence before cleanup.
4. **Treat a stdout prefix that parses as success.** Rejected. Only the supervisor knows the
   stream was longer.
5. **Bound a step's files only after it ends.** Rejected alone. A final refusal does not stop a
   tool from filling the disk meanwhile.
6. **Copy a seed with `shutil.copytree` after its message passed.** Rejected. A message admits no
   file.
7. **Record CLI processes with one `perf record` around a loop.** Rejected. Each execution's
   whole output must be judged by its command's oracle, which only the supervisor of that
   execution observes.

## Decision

1. **The contained Linux mode** (`--mode linux_container --image … --scratch-fs …`).
   - **The shard's preflight.** The run takes the machine lock, then `runner.contained_host`,
     which shards now share: a digest-pinned image, a Linux runtime with cgroup v2, the scratch
     filesystem's bound verified by an enforcement fill, and the environment's CPUs. A refusal
     keeps its reason, and nothing is created.
   - **Every execution is one container.** Each request goes to `toolrun.Contained`, the
     authority supervisor as PID 1 through `supervise.run_contained`: network none, read-only
     root, all capabilities dropped, no new privileges, the PID and memory limits with swap
     disabled, the CPU set, no CPU quota, the container user, and `--ulimit fsize` at the room
     left. Its removal is confirmed. An unconfirmed removal, a changed scratch filesystem or a
     limit the supervisor did not observe as declared fails the run.
   - **Only the run's directory is writable.** The run gets its own root entry, `diagnostic`,
     on the verified filesystem. Writable binds must lie below it, at their own paths.
     Executables, fixtures and admitted evidence are bound read-only. A directory left by an
     earlier run refuses the next as `scratch_in_use`.
   - **Outputs are staged, then admitted.** Tools write below the run's directory. Each staged
     file is enumerated confined and bounded, copied into the evidence and removed before the
     run's cleanup. Staged bytes count against the evidence room.
   - **Callgrind stays local.** Its region switch signals a process in another PID namespace,
     so it is an omission (`callgrind_switch_not_contained`) in this mode.
2. **CLI cases.** Each execution runs the side's own diagnostic AF binary for its command.
   - **Private state.** Every execution gets a new namespace as its HOME, XDG directories and
     TMPDIR, and runs after its declared `setup.sh` step.
   - **Seeds.** A state read copies the side's own seed, written once by that side's writers
     through `setup.sh` and sealed (decision 5).
   - **The whole output is judged.** The supervisor counts the command's patterns and observes
     its JSON stream over the whole output, past any retained log, and `clicases.judge` decides.
     A truncated retained log is not itself a failure there. A rule that needs the document
     whole refuses it.
   - **Profiles and traces.** A CPU profile records each fresh process whole with its own
     `perf record` (`subject: cli_process`, `whole_process`, `fresh_process`), and one
     supervised script covers every recording. A trace's region is `cli_startup` for a discovery
     command and `cli_command` otherwise.
   - **Containment is a capability.** A case that spawns processes runs only in the contained
     mode, and in trusted local mode it is an omission, `requires_linux_containment`.
   - **musl.** `linux_musl_cpu_profile` and `linux_musl_trace` derive from
     `linux_musl_shipping`, keeping its target, toolchain, cargo-zigbuild, Zig, linker and image.
3. **Supervision fails before any oracle** (`toolrun.Result.supervision_failure`). A launch
   refusal, a late reply, outputs over their room, a cancellation, a timeout, a refused
   lifecycle, an unconfirmed cleanup or a truncated stdout fails every seed, probe, samply, DHAT
   and tool run before its oracle. Only the exit code is the oracle's.
4. **All of a step's files together.** A watch measures the evidence, with staged files, every
   50 ms while a tool runs. It cancels the tool through its supervisor once the files outgrow
   the room. A finished step that left the evidence over its room is refused as
   `evidence_over_room`, and everything it wrote is kept and counted, never deleted or clamped.
   Evidence, scratch and build bounds stay distinct.
5. **A seed is admitted before it is used.** After its writer's message passes, the whole seed
   tree is enumerated as confined directories and regular files within the seed bounds, then
   sealed and identified (`seeds.seal`, `seeds.tree`). Each repetition's copy goes through
   `admission.copy_tree`, which reads every file as its enumerated entry within its size and the
   deadline, and the copied bytes must have the sealed identity. The same applies to the first
   preparation and every refresh.
6. **Trace coverage** (`af.perf-profiles/4`).
   - **Every trace states its coverage.** It names its subject, its region phase, how many
     region spans it holds, and its coverage: `complete`, `partial` (spans dropped or left
     open) or `region_missing`.
   - **A setup-only trace is never complete.** A setup that filled the bounded recorder leaves
     `region_missing`, and the page says so.
7. **The real-tool fixture.** `fixtures/performance/diagnostic/real-tool-synthetic/` holds real
   perf 6.1 records over an owned synthetic C workload, with only the machine-specific DSO
   prefix normalized, as its README records. It restores parser and stack-quality coverage. It
   is not an AF capture and claims nothing about one.

## Consequences

- **Gate repairs.**
  - **The test helper.** `diagnostic_tools.rs` resolves the workspace through
    `AF_WORKSPACE_ROOT` first. The static guard is unchanged.
  - **The hand-off.** `Session::pause` showed its line, then flushed input, then went raw. An
    Enter typed as soon as the line appeared could be flushed, and the wait never ended. It now
    goes raw and flushes before it shows the line.
  - **The TUI test.** It waited for any matching exit line. A repeated command leaves earlier
    ones on the main screen, so it could send Enter while the command still ran. It now waits
    for the line written after its own keys. The eight repetitions and every assertion stay.
  - **A pty regression.** It holds the terminal's output full while `pause` writes, and requires
    raw mode and an empty input queue by then. It fails deterministically under the old order.
- **Tests.**
  - **The real supervisor** (`diagnostic_tools.rs`). A 65,546-byte stream whose 65,536-byte
    prefix parses fails. Two 16-byte files plus 3 bytes under a 16-byte room are refused, with
    35 bytes counted. A writer of many small files is cancelled while it runs.
  - **Stand-in children.** A truncated probe stdout, a symlinked seed, aggregate room, CLI
    traces with a private HOME and a failed oracle, a state read refused in trusted local mode,
    a CLI CPU profile, a setup-only trace, and confined copies.
  - **Stand-in containers.** A stand-in container runtime checks the contained driver: the
    flags, writable binds only in the run's directory, a seeded `task show` of about 1.7 MB
    judged by its stream rules (and failed by one missing sequence number), staged captures and
    confirmed removals. The stand-in isolates nothing.
  - **Fixtures.** Four new profile fixtures cover version 4.
- **Still pending.**
  - **A qualified contained capture.** It needs a Linux executor with a verified 2-GiB
    disk-bounded scratch filesystem, which this host does not have.
  - **perf in a container.** The image must run perf. Docker's default seccomp profile refuses
    `perf_event_open` without a capability, and the capability check then reports perf
    unavailable with that reason. Nothing adds a capability, a profile or a host setting.
  - **Operational evidence.** Real CLI captures, trace overhead and the AF capture of the real
    script remain package p8's.
- **Unchanged.** The 15-second lease, the 8-MiB envelope cap, every verdict bound, the supervisor
  request and result contracts, and the trace-on/off identity of the default build.
