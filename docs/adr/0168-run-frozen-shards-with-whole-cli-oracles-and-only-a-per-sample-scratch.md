# ADR-0168: Run frozen shards with whole-CLI oracles and only a per-sample scratch

**Status:** accepted (2026-10-19); decisions 5, 6, 7 and 10 amended by
[ADR-0169](0169-coordinate-a-machine-seed-each-side-once-and-build-linux-recipes-in-a-pinned-image.md)
(one machine lock, side-written seeds, the Campaign fixture and executable states). Package p3b of the
[continuous performance laboratory](../design/continuous-performance.md), with the closeout of
three findings of the unverified p3a repair
([ADR-0167](0167-bound-every-supervisor-wait-and-verify-only-what-was-observed.md)). Amends
decision 7 of
[ADR-0166](0166-build-frozen-plans-and-supervise-samples-with-one-authority-instrument.md) (the
writable scratch bind) and decision 3 (adapter overlays), and makes the supervisor request
`af.perf-supervisor-request/2`, its result `af.perf-supervisor-result/3`, the sample
`af.perf-sample/2` and the shard record `af.perf-shard/2`. Reports, archive, profiles and CI stay
later packages.

## Context

Package p3b must execute frozen plans: every planned slot of a shard, in order, under the
authority supervisor, with independent correctness oracles for the whole AF command line, health
sentinels, host identity and complete evidence. The predecessor's sealed output was not verified,
and root checks reproduced three defects in it:

- **The integration test mistook a target directory for a triple.** `perf_supervisor.rs` took
  any profile directory whose parent had no `CACHEDIR.TAG` for `<target>/<triple>/<profile>`.
  The kernel's precreated target `cargo_target` and a valid `--target-dir target/linux-checks`
  have no tag, so the test asked cargo for `--target cargo_target`, and all nine supervisor
  tests failed before any sample. Cargo also writes that tag inside `<target>/<triple>/`, so the
  heuristic was wrong both ways.
- **A build script could rewrite the overlaid probe.** `Builder._side` recorded the authority
  closure's hashes but only checked that the changed paths were a subset of the overlay's.
  A synthetic fake compiler that rewrote a helper during the compile produced an artifact with
  the mutated bytes, recorded the authority hashes, and reported `source_unchanged`.
- **The whole scratch filesystem was writable.** The container bound the dedicated
  filesystem's root at `/perf/scratch-fs` and gave the child `/perf/scratch-fs/sample`. A real
  PID 1 supervisor run showed a child's `../escaped-scratch-file` surviving a cleanup reported
  clean, because only `sample/` was cleared.

Running whole-CLI cases raised two more problems:

- **Fixtures must be written inside the containment.** A CLI Task or state fixture is written
  by the side's own binaries, which are untrusted. The supervisor accepts only an empty scratch
  and clears it itself, so the host cannot prepare one.
- **Outputs can exceed the retained-log limit.** `af task show --json` over 10,000 events prints
  about 1.6 MB, more than the 1-MiB hard cap on retained output.

## Considered options

1. **Keep inferring the target layout from the directory tree.** Rejected: no marker file or
   name distinguishes a precreated target from a triple directory.
2. **Record cargo's own layout in a build script** (its executable lives in the host layout,
   `OUT_DIR` in the target layout). Chosen: the relative triple directory survives a relocated
   or cache-restored target.
3. **Hash the published probe binary instead of its inputs.** Rejected: an expected hash of a
   binary does not exist before the build.
4. **Prepare fixtures on the host and copy them into the scratch.** Rejected: the side's
   binaries that write them would run uncontained, and the scratch would no longer start empty.
5. **Time a wrapper that prepares the fixture and then executes AF.** Rejected: setup would be
   inside the child-lifetime metric.
6. **Count declared patterns over the whole stream in the authority supervisor.** Chosen for
   output larger than any retained log. Raising the 1-MiB cap would contradict the resource
   contract.
7. **Bind only the per-sample directory.** Chosen: the filesystem's root and `lost+found` are
   never in the container, and the bind's mount root cannot be renamed.

## Decision

1. **The integration test reads cargo's layout.** `crates/review-process/build.rs` compares its
   own executable's host layout with `OUT_DIR` and emits `REVIEW_PROCESS_TARGET_TRIPLE_DIR`:
   empty for `<target>/<profile>`, the triple for `<target>/<triple>/<profile>`.
   `tests/perf_supervisor.rs` derives the target directory and `--target` from it. It refuses a
   profile directory outside the recorded triple and still asks the same cargo for the example,
   offline and locked, in its own profile.
2. **The overlay is verified around every compile.** Each closure file is written fresh
   (`O_EXCL`, no symlink followed) and made read-only (0444). Before and after every compile
   attempt, whatever its status, each file must be a regular file of mode 0444 holding exactly
   the authority's bytes. Otherwise the build refuses with `overlay_integrity` (exit 4) and keeps
   its logs, writes no `builds.json` and publishes no probe. This is never a compile outcome, so
   no later adapter is tried. Restoring a failed adapter's overlay recreates the source's
   original bytes and mode. `builds.json` stays `af.perf-builds/3`.
3. **Only the per-sample directory is writable.** `container_argv` binds `<scratch-fs>/sample`
   at `/perf/scratch` below the read-only root, and refuses any other writable bind and any
   read-only bind into the scratch. Before each sample, `prepare_scratch` requires:
   - the filesystem root to hold only `lost+found`, which is never touched, and `sample`;
   - `sample` to be the container user's empty 0700 directory, created when absent;
   - the container user to be this user, unless the host is root and hands the directory over
     explicitly. No unstated host/container uid relation is assumed, because the host must list
     `sample` afterwards.

   After the sample, `scratch_problems` compares every root entry's identity and mode, then
   requires `sample` empty again. An added entry, a replaced or renamed `sample`, or a
   non-empty one stops the shard. The fill-verified capability check is unchanged and runs at
   the start of every container shard.
4. **`af.perf-supervisor-request/2` and `af.perf-supervisor-result/3`.**
   - **Fixture setup.** A request may name a `setup` command (argv, environment, cwd,
     deadline). It runs first, untimed, under the same containment, in the same scratch.
   - **The measured child** launches only after the setup exits 0, with its exit observed
     unreaped and a verified lifecycle (pipes-only `no_observed_descendants` in trusted local
     mode). Otherwise the result is `setup_failed`, nothing is measured, and the lifecycle is
     the setup's.
   - **The setup's record** holds its termination, untimed `wall_ns`, output counts and a
     bounded stderr tail; its output is never part of the payload.
   - **Stdout patterns.** Up to eight `stdout_patterns` of at most 128 bytes are counted by
     `review_process::sample` over the child's entire stdout, retained or not, across read
     boundaries. They are reported as `stdout_pattern_counts`.
   - **Stdout cap.** The cap may now reach the 1-MiB log cap, so a CLI document up to the
     plan's sample log size is retained whole.

   A setup failure is the side's `launch_failure`; an uncertain setup cleanup stops the shard.
   The old versions are refused.
5. **Whole-CLI oracles (`fixtures/performance/cli.toml`, `af.perf-cli/1`).** Every adapter
   declares, for every instance of a registry `process` case, either a runnable command or why
   it is pending. A runnable command has the actual AF argv with the declared placeholders
   `{af}`, `{probe}`, `{scratch}` and `{package_version}`, a named setup, and rules:
   - an exit code;
   - stdout lines and contained tokens over the retained output;
   - JSON field equality, pattern, count, contiguity and existence over a whole retained
     document;
   - exact pattern counts over the entire stream.

   Empty output never passes, and an exit status alone is refused. A side's package version is
   read from that side's `Cargo.lock` in the plan's Git objects, whose bytes must match the
   plan's digest; never from the binary. Each side gets the first adapter whose `min_version`
   covers it. A side no adapter covers has no CLI coverage. Rules come from the declared
   fixture and the documented commands, never from observed output. The commands:
   - the four discovery commands;
   - `task start` without execution;
   - `task run --execute` of RootInputs, check(`/bin/true`), a command evaluator and acceptance.
     It must finish `verified`, with a check span `truth` recorded for `root.nodes.checks`
     proving the check ran;
   - `task show --json` on exactly 10,000 Task events. The supervisor counts 10,000 history
     entries, exactly one at sequence 9999, none at 10,000, and 9,997 lease renewals.

   `review ledger` (semantic text, no `--json`) and `review report --format json` are pending:
   no writer of this package creates a matching 10,000-event Campaign through each side's own
   writers, so they are unavailable rather than measured on another fixture.
6. **Side-written fixtures.** `cli/setup.sh` is the authority's reviewed setup.
   - `task_repository` makes a fresh Git repository of `cli/task-roots/repo` and its Task file.
     Its catalog pins are the reviewer-package digests computed by the independent reference
     encoding, which a test checks.
   - `task_started` then captures the Task with the side's own `af task start`.
   - `state_seed` runs the side's own probe with the new fixture-writer contract
     `cli_state_fixture_v1`, which writes the Task log through the protected public Store writer.
     The script requires the probe's message to report it passed with exactly 10,000 events.

   Every sample writes its fixture afresh; setup time is reported apart from the child interval.
7. **`cli.py run` executes exactly one frozen shard.**
   - **Inputs.** It verifies the build's artifacts and rehashes each binary before every launch,
     so shards never rebuild. It reads the authority's registry, its golden entries (checked
     against this driver's reference) and its CLI registry and fixtures from the plan's Git
     objects.
   - **Refusals before sampling.** It refuses a plan whose frozen environment, recipe or case
     states say not executable, a mode other than the environment's (trusted local process
     group, or one pinned Linux container per sample), a host of another OS or architecture,
     and a second shard on the same host and user.
   - **Execution.** Slots run in planned order with no retry, replacement or adaptive count.
     An instance this host or build cannot cover (no binary, no CLI adapter, a pending command)
     is skipped, never substituted, and the shard ends `failed` with `unavailable_coverage`.
   - **Stops.** These stop the shard (`failed`): an uncertain cleanup, an exit not observed
     unreaped, a supervisor without a valid reply (no record for that slot), a changed artifact,
     an unconfirmed container removal, a scratch not restored, cgroup limits other than the
     environment's, and a driver error.
   - **Deadlines.** Before each slot, its worst case is checked against the earlier of the
     shard's sampling deadline and `--deadline-unix-ms`; the rest stays unstarted
     (`interrupted`), as after a termination signal.
   - **Evidence.** Records are appended as they are made. Whatever happened, the shard writes a
     terminal `shard.json` and its manifest, so partial evidence is preserved and missing slots
     are counted. Failed samples keep their capped output under `logs/` within half the bundle
     budget.
8. **`af.perf-sample/2` and `af.perf-shard/2`.**
   - **A sample** adds its identity (case, contract, adapter, parameters, fixture digest),
     each metric's scope, the oracle's kind and detail, the execution (containment,
     termination, exit code or signal, exit observation, lifecycle, the host's wall around the
     supervisor and the untimed setup) and its retained log.
   - **A shard** adds its execution (mode, containment, pinned image, supervisor and
     `builds.json` hashes, sampling deadline, chosen CPUs), its unstarted and failed slot
     counts, retained log bytes, and the host identity and start/end interference.
   - **Host observations.** Each is a value or explicitly unknown with its reason: CPU model
     and topology, kernel, OS image, libc, memory, virtualization, microcode, container runtime,
     affinity, quota, cache, ASLR and Python; load, steal, pressure, throttling and cgroup memory
     events.
   - **`compare` refuses as integrity failure**: a shard whose mode is not the plan
     environment's, a supervisor or build other than the run bundle's, a record of another
     containment, case or parameters, and a primary metric whose scope is not its timing
     boundary.
   - **`cli.py aggregate`** imports verified shard bundles from other hosts into the run
     directory, never over a different bundle, and recomputes the verdict. The verdict is
     unchanged (`af.perf-verdict/2`).
9. **The shipping recipe builds through its pinned cargo-zigbuild.** `cli.py build
   --zigbuild-bin DIR` takes a directory whose `cargo-zigbuild` and `zig` must report exactly the
   recipe's versions (cargo-zigbuild 0.23.4, Zig 0.15.2). A wrapper of `python3 -m ziglang`
   qualifies. Every product, probe and supervisor step then runs `cargo-zigbuild zigbuild` with
   the same locked, offline, allowlisted invocation; only those two tools precede the verified
   Rust toolchain on PATH. The linker identity is Zig's own, and the isolation record names both
   versions. Without the directory, or with another version of either tool, the build is
   unavailable before anything is created (amending decision 1 of ADR-0166).
10. **Executable states.** The local macOS arm64 environment and its native recipe are now
    `supported`, for CLI discovery and every nonspawning library case, as `partial_scope` runs.
    Every Linux environment and recipe, `source_capture`, and the CLI Task and state cases stay
    `not_yet_executable`. Their contained path is implemented and tested, but no host with a
    verified disk-bounded scratch has run a shard. The CLI Task oracles are `available`; the CLI
    state oracle stays pending for its Campaign fixture.

## Consequences

- **Validated on the development host** (macOS arm64, Rust 1.88.0, Python 3.15):
  - The supervisor's 14 integration tests pass in a precreated nested target directory without
    `CACHEDIR.TAG`, with and without an explicit `--target`; before this change that layout
    failed all of them.
  - The overlay regression test, a build script that rewrites the overlaid helper, fails on
    the old builder and passes on the new one.
  - The laboratory's 249 Python tests pass, including the shard, CLI oracle, scratch inventory,
    host observation and zigbuild tests, on macOS Python 3.9.6 and 3.15 and on Linux Python
    3.9.25 and 3.11.17.
- **A real same-source shard run.** A `partial_scope` plan of ten macOS instances (four
  discovery, four small canonical/CAS, two authority checks; N=12, K=10, two shards) was built
  from an owned Git fixture of this tree and run shard by shard in trusted local mode:
  - Both shards completed with every record and sentinel slot (320 of 320), no failed sample
    and healthy sentinels.
  - The verdict is `incomplete`, because two discovery instances are statistically
    inconclusive at N=12 on a host whose load average was 13 to 23.
  - This is evidence of the instrument's function on one development host, not a calibrated
    pass or a speed claim.
- **Every runnable CLI command with real binaries.** The release `af`, probe and supervisor of
  that build ran each command with its setup through the real supervisor, in trusted local
  mode, from a validation copy whose check program was `/usr/bin/true` (macOS has no
  `/bin/true`). All oracles held:
  - The four discovery commands.
  - `task start`.
  - `task run --execute`, after the side's own `task start` setup, with the `truth` check span.
  - `task show --json` on 1,629,418 bytes of output, with pattern counts 10,000, 1, 0 and 9,997,
    after a 22.5-s probe-written setup.

  This exercised the Linux-only fixtures and oracles outside containment; it was not a sample.
- **Real Linux mount semantics.** A static aarch64 musl `perf-supervisor` built from this tree
  ran as PID 1 of a real Linux container (Docker Desktop 27.4, a pinned local image) through
  `run_contained`, with only `sample/` bound:
  - The child's write to `../escaped-scratch-file` and both renames of the mount root were
    refused.
  - The lifecycle was clean, the scratch was cleared from inside, the container was removed,
    the observed cgroup limits matched the profile, and the filesystem's root was unchanged.
  - A control run with the old whole-filesystem bind let the escape and the rename succeed, and
    the host check reported both.

  The bound directory was a shared host directory, so this checked mount semantics, not the
  2-GiB bound or a shard.
- **A real shipping-recipe build.** Inside a local Linux arm64 image with Rust 1.88.0,
  cargo-zigbuild 0.23.4 and Zig 0.15.2, without network, `cli.py build --zigbuild-bin` built a
  same-source plan of the musl shipping recipe from an owned Git fixture. It used a private
  Cargo home seeded only with registry archives and the index.
  - The supervisor, `af` and probe built as static x86-64 ELF executables, and the embedded
    commit was verified.
  - The linker identity was Zig 0.15.2, and the isolation record named both tool versions.
  - The resulting `af --version` printed `af 0.12.0-rc.3` under amd64 emulation.

  This was a build, not a shard or shipping evidence of the hosted x86_64 environment.
- **Not exercised here.** Linux containment of a shard: this host has no verified disk-bounded
  scratch, so `run --container` refuses here before any container starts. Validating the
  contained path, the Campaign fixture writer, seed reuse for the 10,000-event state fixture,
  hosted x86_64 shipping evidence and the five-day calibration remain pending.
- **Setup cost.** Each CLI state sample writes its 10,000-event fixture afresh (about 20 s of
  untimed setup on this host). That honest cost can exceed quick-tier shard deadlines, which
  then leave slots unstarted and the run incomplete. Nothing is dropped and no deadline is
  weakened.
