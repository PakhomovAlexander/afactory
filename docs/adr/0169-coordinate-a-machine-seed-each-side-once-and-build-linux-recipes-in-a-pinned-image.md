# ADR-0169: Coordinate a machine, seed each side once, and build Linux recipes in a pinned image

**Status:** accepted (2026-10-19). Completes package p3b of the
[continuous performance laboratory](../design/continuous-performance.md) after the unverified
snapshot of
[ADR-0168](0168-run-frozen-shards-with-whole-cli-oracles-and-only-a-per-sample-scratch.md), whose
decisions stand except where this one amends them:

- decision 5 (`review ledger` and `review report` are no longer pending);
- decision 6 (side-written fixtures become seeds);
- decision 7 (the per-host lock);
- decision 10 (executable states);
- decision 1 of
  [ADR-0166](0166-build-frozen-plans-and-supervise-samples-with-one-authority-instrument.md)
  (host-only builds).

It makes the CLI registry `af.perf-cli/2`, the shard record `af.perf-shard/3` and the build
record `af.perf-builds/4`. Reports, archive, profiles and CI remain later packages.

## Context

Root checks of the predecessor found five problems:

- **F1, host serialization.** The shard lock lived in `tempfile.gettempdir()`, so two processes
  of one user with different `TMPDIR` values both held "the" lock, and the container preflight
  (a 2-GiB fill of the shared scratch filesystem) ran in `Shard.__init__`, before `run` took it.
- **F2, the gate's fixture mutation test.** `shutil.copytree` keeps a sealed checkout's read-only
  modes, so the test that edits its own copy of the Task fixture failed with `EACCES` and the
  mandatory gate never reached the Rust tests.
- **F3, missing workloads.** `review ledger` and `review report` had no 10,000-event Campaign
  fixture written by each side's own writers. Every `task show` sample rewrote its 10,000-event
  fixture (about 20 s of setup), which can exceed quick-tier deadlines.
- **F4, container builds.** Any recipe naming a container image was refused unconditionally.
- **F5, capability states.** Every Linux environment and recipe, `source_capture` and the CLI
  Task and state cases were globally `not_yet_executable`. A capable host could not run them
  without editing the authority, and the registry state was standing in for a host's runtime
  refusal.

## Considered options

1. **Keep a per-user lock below `TMPDIR`, or move it below `HOME` or the run directory.**
   Rejected: every one of these differs between two processes of one machine.
2. **One lock file at a fixed system path, shared by every user.** Chosen. The directory is the
   system's own `/tmp`. A per-user lock would let two users run concurrent shards on one host,
   which the design forbids.
3. **Delete and recreate a stale lock file.** Rejected: removing a live lock's inode lets a
   second process lock a new file while the first still holds the old one. `flock` locks are
   released by the kernel when their holder dies, so no lock file is ever stale.
4. **Write the Campaign fixture with raw `EventStore::append` calls.** Rejected: that is not a
   supported writer, and a Campaign written that way would not be the state the product writes.
5. **Write the Campaign through 10,000 operator commands of the CLI.** Rejected: the cost is
   minutes of process startup per side, and the side's event count before the operator commands
   is not known.
6. **Write the review through the side's own `af review run`, then top it up through that side's
   public Findings Ledger writer.** Chosen. The review is the product's own whole-CLI writer.
   The top-up uses the writer `af review group` and `af review ungroup` use, through the side's
   own probe. The probe counts the log through the side's Store, so the total is exact
   whatever the review itself wrote.
7. **Rewrite every large fixture in every sample.** Rejected: its setup cost alone can exhaust a
   quick shard.
8. **Write each side's state once per shard, keep it immutable, and copy it into each sample.**
   Chosen. No sample shares mutable state, and the setup stays apart from the child interval.
9. **Pin a build image digest in the registry now.** Rejected: no reviewed, published image
   exists yet. The only local image is a validation aid, and pinning it would make the
   authority name an image no hosted runner can pull.
10. **Fall back to a host build when no image is configured.** Rejected: that silently replaces
    filesystem isolation with trust.

## Decision

1. **One machine coordination lock** (`perflab/hostlock.py`). A shard and an enforcement fill
   (`capabilities --verify-scratch-enforcement`) take an exclusive `flock` on
   `/tmp/af-perf-machine.lock`, resolved through the system's `/tmp`, before anything shared is
   touched:
   - `run_shard` takes it before reading the build, creating the run directory, filling the
     scratch filesystem or starting a container;
   - it is held until the terminal `shard.json` and manifest are written, and released on every
     exit;
   - `shard.json` names the lock in `execution.coordination_lock`.

   The policy is explicit:
   - one lock per machine for every user;
   - the directory must be root-owned, and sticky when world-writable;
   - the file is created `0644` and opened read-only, without following a symlink;
   - it must be a regular file with one link, writable by its owner alone;
   - after locking, its name must still be the locked inode;
   - it is never written, truncated, replaced or removed.

   Another holder refuses `concurrent_shard`; anything unsafe refuses `coordination_unavailable`.
   Neither runs concurrently. Concurrent shards need separate machines.
2. **The fixture test edits only its own copy.** `tests.support.writable_copy` copies a committed
   fixture into the test's directory and makes only that copy owner-writable. A test edits a
   copy of the Campaign fixture's pipeline the same way. Nothing else changes: the sealed tree
   and `.af` stay read-only.
3. **The Campaign fixture.**
   - **The repository.** `cli/campaign-roots/repo` is a model-free review: a pinned whole-tree
     pipeline whose shell reviewer reports one minor Finding per line of `src/fixture.txt`
     (100). A test checks the pipeline pin against the reference content digest.
   - **The review.** The setup runs the side's own
     `af review run --campaign perf-campaign --pipeline .af/pipelines/review.toml
     --policy-rev HEAD` in its private scratch.
   - **The top-up.** The side's own probe, under the new writer contract
     `cli_campaign_fixture_v1`, opens that state through the side's public Store. It groups the
     second Finding under the first and releases it again, pair after pair, through `Ingest`,
     until the Campaign log holds exactly 10,000 events. An odd remainder first advances the
     policy clock once through `Ingest::advance_policy_time`. The Ledger therefore ends as the
     review left it.
   - **The count.** The probe re-reads the log through a fresh Store, and the setup requires
     its message to report exactly 10,000.
   - **Every event is relevant.** The Ledger projection folds every one of them.
4. **The ledger and report oracles** (`af.perf-cli/2`). The rules come from the documented
   output and the declared fixture:
   - `review ledger --campaign perf-campaign --state {scratch}/seed/campaign`, which has no
     `--json`, must print exactly 100 lines. They are the fixture's open, minor, ungrouped
     Findings: the first and the hundredth, and 100 Finding keys.
   - `review report ... --format json` must be `af/review-report@4` of `perf-campaign`: one
     recorded run, Ledger round 1, 100 Findings, all of them open.
   - Empty output, a wrong count or a wrong status fails the side that produced it.
5. **Seeds.** `cli.toml` declares seeds (`task_state`, `campaign_state`), each with its writer
   and exact event count. A command reads one through the `seed_copy` setup.
   - **Written once.** Before a side's first sample that needs a seed, the shard runs one
     supervised request in the shard's containment:
     - its setup is the side's own writer (`setup.sh seed_task_state` or
       `seed_campaign_state`);
     - its child publishes what was written (`seed_publish`).

     A seed is published only after every process of its writer has exited, so no Store handle
     stays open. It is then sealed (directories 0555, files 0444) and identified by one digest
     over every entry's path, kind, mode and bytes.
   - **Copied privately.** Each sample copies the sealed seed into its own scratch in its
     untimed setup. The shard verifies the digest before and after every sample, so a changed
     seed stops the shard (`seed_changed`).
   - **Where seeds live.** In a container, the seed store is the verified scratch filesystem's
     `seeds/` directory. The writer binds only its own new directory there, writable, at
     `/perf/seed-out`, and a sample binds its seed read-only at `/perf/seed`. In trusted local
     mode the seed store is a private namespace of the shard's scratch parent.
   - **Failures.** A failed writer fails that side's samples (`launch_failure`) and is never
     retried. Seeds are removed when the shard ends; a removal that fails fails the shard.
   - **Evidence.** `af.perf-shard/3` records every seed: side, writer, binary hashes, digest,
     size, setup time, termination, lifecycle, samples served and whether it stayed unchanged.
     `compare` requires every passing CLI state sample to name a ready seed of its own side.
   - **Cache properties.** Each sample is a fresh process on a fresh copy of its side's Store,
     application-cold; the OS page cache is warm from the copy.
6. **Linux recipes build in a pinned build image** (`af.perf-builds/4`). The image is the
   recipe's own `container_image`, or one configured with `cli.py build --build-image`. It is
   pinned by digest (an image ID or `name@sha256:`), present locally, and never pulled
   (`--pull never`); a configured image must equal the recipe's pin.
   - **Toolchain.** Rust, cargo-zigbuild and Zig are the image's, verified there by exact
     version. Zig may be reached as `python3 -m ziglang`, as cargo-zigbuild itself does.
   - **Compiles.** Every compile is a named container run as this user:
     - no network, a read-only root, no capability, no new privileges;
     - started through `env -i` with exactly the allowlisted variables, whatever the image
       declares;
     - only the run's own directories mounted, at their host paths: the step's worktree, the
       private repository (read-only), the run's HOME, TMPDIR and Cargo home;
     - each overlaid authority closure file bound read-only over its overlay. The integrity
       check of ADR-0168 still runs around every compile.
   - **Dependencies.** `cargo fetch --locked` runs once per worktree in a container of the same
     image. It has network and nothing but the run's directories, so no host credential, Git
     configuration or provider state is reachable. An operator's `--cargo-home` must hold no
     credentials or configuration.
   - **Cleanup.** Each container's removal is confirmed, and an unconfirmed removal refuses the
     build.
   - **Trusted host builds.** A Linux recipe builds on the host only with an explicit
     `--trusted-host-build`. It is recorded as `filesystem: trusted_host` and claims no
     isolation. Without an image or that flag the build refuses (`build_image_required`);
     nothing falls back.
   - **What `builds.json` records.** The isolation kind, the image (reference, ID, platform),
     the compile network, the dependency fetch and the filesystem boundary.
   - **Checked by `compare`.** A recipe that pins an image must have been built in exactly it.
7. **Executable states.** Every recipe, environment and case in `suite.toml` is `supported`.
   The CLI state oracle is `available`. A Linux recipe without a pinned image is valid because
   its build image is configured and recorded.
   - **Host refusals.** A host that cannot provide a capability refuses at run time with its
     reason: a container runtime, a verified 2-GiB scratch, a matching OS and architecture, a
     configured build image. `capabilities` lists these reasons, and they never become a
     registry state.
   - **Still declared.** Every critical instance stays declared.
   - **Pending outside the registry.** Operational acceptance on hosted runners and the
     five-day calibration cohort.
8. **Refusals precede creation.** A shard checks its mode, host, CLI registry and container
   preflight before it creates the run directory.

## Consequences

- **Versions.** `af.perf-cli/1`, `af.perf-shard/2` and `af.perf-builds/3` are refused with no
  reader; their schemas are replaced by `performance-cli-v2.json`, `performance-shard-v3.json`
  and `performance-builds-v4.json`. The synthetic comparison bundles were regenerated.
- **Seed cost.** Seeding is paid once per side and seed in a shard, not once per sample. On
  the development host a Task seed took 21 s and a Campaign seed 64 to 78 s natively. The
  per-sample copy (8.9 MB, or 9.8 MB in 313 files) is setup, reported apart from the child
  interval.
- **Validated on the development host** (macOS arm64 with Docker Desktop 27.4's Linux arm64
  VM, Rust 1.88.0):
  - **The laboratory's tests.** 265 tests pass on a read-only copy of this tree, as the gate
    sees it, under Python 3.15, 3.9.25 and 3.11.17.
  - **The machine lock.** Processes with different private `TMPDIR`, `HOME` and XDG roots refuse
    each other at the real lock, and a contended shard refuses before any docker call or run
    directory.
  - **The probe's integration tests.** All 23 pass, and the 14 supervisor integration tests
    pass in a precreated nested target without `CACHEDIR.TAG`, with and without an explicit
    `--target`. A first `perf_probe` run alongside the full Python suite lost one authority
    sample to a production lease that expired under that load; the same tests passed alone.
    No lease or deadline was changed.
- **A real Campaign through each side's writers.** The release `af` reviewed the fixture
  repository in 1.9 s, with 100 Findings, and the release probe topped its log up to exactly
  10,000 events in 24 s. `review ledger` printed the 100 Finding lines in 0.41 s and
  `review report --format json` reported 100 open Findings in 0.35 s.
- **A real container build of the musl shipping recipe.** It ran in the pinned local image
  `sha256:62e76ea1…` (Rust 1.88.0, cargo-zigbuild 0.23.4, Zig 0.15.2 through
  `python3 -m ziglang`) from an owned Git fixture of this tree:
  - the dependency fetch was a container with network;
  - every compile was a container without it;
  - `af`, the probe and the supervisor are static x86-64 executables;
  - the embedded commit was verified, and that `af --version` printed `af 0.12.0-rc.3`
    under amd64 emulation.

  On this host, separate containers share a target directory bind-mounted from macOS. In 3
  of 5 container builds a probe compile failed with `E0463 can't find crate` for an rlib that
  the previous step's container had written there. Each failure is recorded as a failed
  adapter with unavailable coverage, and nothing is substituted. Native Linux bind mounts
  were not exercised here.
- **The same-source frozen plan at nightly defaults.** It ran on the supported native subset
  (four discovery and four small canonical/CAS instances, N = 40, four shards) in trusted
  local mode:
  - 752 of 752 records, every sentinel healthy and no failed sample;
  - the verdict is `incomplete`: two discovery instances are statistically inconclusive on a
    host whose load average was 40 to 71.

  This is the instrument's function, not a calibrated pass or a speed claim.
- **The CLI cases in real containers.** A plan re-targeted to this host's native aarch64 ran
  in Linux sample containers (supervisor as PID 1, pinned arm64 sample image, cgroup limits
  verified):
  - discovery, `task start`, `task run --execute` with check(`true`) proved, and `task show`,
    `review ledger` and `review report` on seeds the containers themselves wrote;
  - 188 of 188 records, every lifecycle clean;
  - the scratch was restored to `sample/` and the seeds removed.

  The scratch was a plain bind, not a verified filesystem, so this is functional evidence.

  An earlier x86-64 run of the same check under Rosetta found two problems:
  - `cp -R` of the read-only fixtures could not be made writable on Docker Desktop's file
    sharing, which `setup.sh` now avoids by copying fresh files;
  - Rosetta writes `$HOME/.cache` into the scratch, which the probe then refuses. That is
    an emulation artifact.
- **Every seeded CLI command natively in trusted local mode.** It ran with the real release
  binaries on a plan given the macOS profile: 160 of 160 records, every oracle holding.
- **Refused here, with reasons.** A contained shard of a hosted Linux plan refuses
  (`host_mismatch`, exit 4) before creating anything, and `capabilities` names the missing
  verified scratch.
- **Validation, not acceptance.** None of this is calibrated performance evidence,
  disk-bounded Linux containment of a shard, hosted x86_64 shipping evidence or the five-day
  calibration; those remain pending outside this change.
- **Not done here.** Seeding the native probe's own 10,000-event library projection fixture
  (`state_projection_v1`) would change that probe contract and its golden entries, so each
  state projection sample still writes its log as probe setup.
