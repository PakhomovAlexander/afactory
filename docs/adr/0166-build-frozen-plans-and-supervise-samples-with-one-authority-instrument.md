# ADR-0166: Build frozen plans and supervise samples with one authority instrument

**Status:** accepted (2026-10-19); decisions 5, 6, 7 and 9 amended by
[ADR-0167](0167-bound-every-supervisor-wait-and-verify-only-what-was-observed.md), which makes
the supervisor result `af.perf-supervisor-result/2` and the capability report
`af.perf-capabilities/2`; decisions 3 and 7 amended by
[ADR-0168](0168-run-frozen-shards-with-whole-cli-oracles-and-only-a-per-sample-scratch.md)
(verified overlays, only the per-sample scratch bound); decision 1 amended by
[ADR-0169](0169-coordinate-a-machine-seed-each-side-once-and-build-linux-recipes-in-a-pinned-image.md)
(Linux recipes build in a pinned build image). Package p3a of the
[continuous performance laboratory](../design/continuous-performance.md), with the p2 closeout.
Supersedes decisions 2 and 3 of
[ADR-0165](0165-own-probe-scratch-in-the-driver-and-take-lease-expiry-only-from-the-store.md)
and the driver execution of decision 6 of
[ADR-0163](0163-measure-native-workloads-through-a-public-api-probe-and-independent-golden-oracles.md).
Paired schedules, shards, sentinels, CLI fixtures and a `run` command stay package p3b's.

## Context

Review of the p2 scratch namespace found three defects that further checks cannot fix.
`Namespace._create` opened the entry `mkdir` had just made by name. A process of the same user
could move that directory away and put an empty 0700 directory of its own at the name, and
every check accepted it. Cleanup removed a cleared child with `rmdir` by name, so an empty
directory swapped in at that name was deleted, and the run still reported success. The tests
of that window expected the deletion. A cleanup note longer than 4,096 characters also
replaced the failed sample's original diagnostic. On Linux (Python 3.9 and 3.11, a 6.10
kernel), a directory descriptor that had been listed once while empty kept listing it as empty.
Cleanup therefore missed `probe-work` and failed every sample.

POSIX has no way to prove that a directory opened by name is the one `mkdir` created, and no
removal by handle. Same-user processes cannot be excluded on a shared host filesystem, so the
real boundary must come from elsewhere.

Package p3a also needs the instrument itself. It must build both products and one supervisor
from a frozen plan without resolving refs again. Measured build scripts must get no
credentials. The authority's probe must compile in measured sources, and a sample's child
interval must be bracketed by the same authority binary on both sides. While implementing that
binary, we found that a child spawned by the standard library inherits the parent's signal
mask: a supervisor that blocks SIGTERM to forward it also blocked the signal in its child, so
a forwarded SIGTERM was never delivered.

## Considered options

1. **Strengthen the namespace checks again.** Rejected: any check followed by a use or a
   removal by name leaves a window for a process of the same user.
2. **Keep the probe's scratch on a host path for Linux samples too.** Rejected: candidate code
   could then race the host's removal of candidate-controlled paths.
3. **Own Linux scratch inside the sample container's PID namespace, and define an explicit,
   non-immune trusted local boundary for macOS.** Chosen.
4. **Keep the Python direct-child runner beside the supervisor.** Rejected: process lifecycle
   rules would then exist twice, and a direct-child kill is not descendant cleanup.
5. **Build in the operator's checkout or reuse its Git configuration.** Rejected: candidate
   build scripts could read persisted authentication headers.
6. **Spawn the child through the standard library and accept the inherited mask.** Rejected:
   signal forwarding would never reach the child.

## Decision

1. **Builds (`perflab.builds`, `cli.py build --plan run.json`).** The builder resolves no
   selector again. A run directory is never reused. It creates a private bare repository
   `<checkout>/target/performance/<run>/git`, fetches exactly the plan's commits into it, and
   refuses to build if its configuration carries any HTTP, credential, URL, askpass, SSH or
   include setting. Detached worktrees `worktrees/{authority,base,head}` each build into their
   own `target/`. Builds use the recipe's toolchain by absolute path (`rustc -vV` must report
   the pinned release) with `--locked --offline --target-dir`, and an allowlisted environment:
   PATH, a private HOME, TMPDIR and CARGO_HOME, global Git configuration disabled, locale,
   offline Cargo and the recipe's flags. Locked dependencies are fetched once, by `cargo fetch
   --locked`, before any build script runs, unless an operator supplies a pre-fetched Cargo
   home. An ancestor Cargo configuration that sets anything beyond `build.target-dir` refuses
   the build; one that sets only `build.target-dir` is overridden and recorded. Each step has
   the plan's deadline, a capped log whose truncation is counted, the worktree byte budget and
   the free-disk floor. A step that exceeds one is killed with its session. Other builders,
   container-image recipes, instrumented recipes, another host OS and missing targets are
   unavailable (exit 4), never substituted.
2. **Each product once; the supervisor once.** The supervisor is built only from the authority
   worktree. Each side builds `af` from its own source. The binary must contain the 12-character
   commit prefix that `crates/af/build.rs` reads through the worktree's `.git` file, and the
   source and Cargo.lock must stay exactly as checked out apart from the overlay. Otherwise the
   side fails with its evidence kept. When baseline and candidate are one commit, every frozen
   input is identical: the candidate references the baseline's artifacts (`shared_with`), and
   both bindings and golden checks are kept.
3. **Adapters (`fixtures/performance/adapters.toml`, `af.perf-adapters/1`).** An adapter is a
   reviewed closure of authority example files, copied at the same paths into the measured
   source of that source's own package. It never replaces a product manifest, a crate source or
   Cargo.lock. Priorities are fixed and unique, at most four per contract, and every adapter
   names its equivalence group. Its closure must be exactly the files the example compiles from,
   which a test checks against the tree. The build tries adapters strictly in priority order,
   records every outcome, chooses the first that compiles, and restores the source after each
   failed one. Nothing is run there, and a runtime or oracle failure never selects another
   adapter. A source with no compiling adapter has unavailable native coverage. The initial
   registry has one adapter, which serves every native contract.
4. **`af.perf-builds/3`.** `builds.json` records the provenance: per side the source, the
   verified embedded commit, the overlay's file hashes, every adapter outcome with its step, the
   chosen adapter and group, coverage, binary and artifact hashes, an unchanged source and
   Cargo.lock, the product step's sanitized argv, environment, deadline, exit status and log,
   and every recipe setting. At run level it records the supervisor and the verified toolchain
   and linker identities. It also records the isolation, both sides' adapter bindings with their
   equivalence, the artifact manifest and its budget, the peak worktree bytes and the cleanup
   outcome. Binaries are copied read-only to `build/artifacts` under a manifest.
   `verify_artifacts` and `verified_binary` rehash them before any launch. `compare` refuses
   library cases whose sides chose different equivalence groups, and a linker other than the
   recipe's. Version 2 is refused (ADR-0145); `run.json` and the other persisted shapes are
   unchanged. The build removes the repository, worktrees, targets and private homes on every
   exit unless asked to keep them; a failed build keeps its logs and `builds.json`.
5. **The supervisor (`review_process::sample`, `examples/perf-supervisor.rs`).** All reusable
   lifecycle code is in the dependency-neutral leaf. The example only parses, refuses and
   reports. It reads one canonical `af.perf-supervisor-request/1` of at most 64 KiB, refused
   with exit 2 and no output when unknown, unbounded or not canonical. It writes one
   `af.perf-supervisor-result/1` line followed by exactly the retained stdout and stderr bytes
   that line declares. The child is spawned with `posix_spawn`, in its own process group, with
   an empty signal mask and default dispositions, and with an exact environment in the
   requested directory. A monotonic clock brackets the launch through the exit, which is
   observed without reaping (`waitid(WNOWAIT)`, kqueue `NOTE_EXIT`). Supervisor startup, the
   lifecycle checks, kills, reaps and pipe closure are recorded apart from that interval. Both
   pipes are drained concurrently into caps, counting every byte past them. Exit, signal,
   timeout, cancellation and launch failure are kept apart. SIGTERM, SIGINT and SIGHUP sent to
   the supervisor are forwarded to the sample and cancel it. `getrusage(RUSAGE_CHILDREN)` before
   the launch and after the reap gives the reaped child's CPU, faults, switches and maximum RSS,
   converted from kilobytes on Linux and never summed. On Linux, raw cgroup v2 files, CPU
   affinity and SMT siblings are reported before and after, each value or its unavailable
   reason. The scope is the container's cgroup, supervisor included, and `memory.peak` is never
   called RSS.
6. **Lifecycle.** With `pid_namespace` containment the supervisor must be PID 1 of a Linux PID
   namespace, and otherwise refuses. At the child's exit it enumerates every other process, and
   live ones fail the sample (`unexpected_descendants`). `kill(-1, SIGKILL)` ends every process,
   including new sessions, then the supervisor reaps and rescans until only it remains. A
   remainder, or a pipe still held, makes the cleanup uncertain, and the driver stops the shard.
   `process_group` containment kills the group while the child is unreaped. On Linux it
   enumerates the group. On macOS it reports `pipes_only` and `no_observed_descendants`, never
   `clean`, and it never claims namespace containment.
7. **Linux sample container (`perflab.supervise`).** `container_argv` builds a named
   (never `--rm`) container from an image pinned by digest, with the supervisor as PID 1. It has
   no network, a read-only root, read-only supervisor and binaries, all capabilities dropped,
   no new privileges, 256 PIDs, 4 GiB of memory with memory-swap equal to it, the declared
   logical CPUs as a cpuset (separate physical cores preferred, sharing recorded), no CPU
   bandwidth quota, a non-root user, and private cgroup and IPC namespaces. Its one writable
   path is the per-sample scratch filesystem: the supervisor requires it empty before the
   launch, clears it from inside only after the namespace is verified empty, and the host
   removes no candidate-controlled path. The scratch must be a dedicated, mounted, disk-backed
   filesystem no larger than the 2-GiB limit, with enforcement verified by filling it
   (`cli.py capabilities --scratch PATH --verify-scratch-enforcement`). tmpfs, the root layer,
   an unbounded bind mount, `statvfs` or a host disk image alone never qualify. Observed cgroup
   limits are checked against the environment and are never assumed. The host's 10-GiB
   free-disk floor is unchanged, and only the fixture's `AF_STORAGE__MIN_FREE_BYTES` is 256 MiB,
   recorded on both sides. `run_contained` runs one sample this way. It removes the container
   afterwards and confirms the removal; an unconfirmed removal stops the shard. Container
   preparation and teardown are returned as host time apart from the child interval.
   `optional_metrics` turns the supervisor's rusage and `memory.peak` into the registry's
   optional metrics as exact decimals, or names the reason each is unavailable.
8. **Trusted local mode and the diagnostic probe.** `cli.py probe` now requires
   `--trusted-local`. It takes the supervisor and probe from a verified build
   (`--build --side`) or explicitly, and runs them under the supervisor with `process_group`
   containment. A case that spawns processes is refused before anything is created (exit 4):
   spawning cases require Linux. The scratch boundary is the caller's private parent, owned by
   this user and writable by no one else (the sticky exception is gone), so no other user can
   touch it. Processes of this same user are trusted and are not excluded: every sample
   reports `same_user_isolation: not_provided`. Inside that boundary the namespace is listed
   through a new open description of its handle every time, which fixes the Linux enumeration.
   An entry is removed only after it has been renamed to a fresh random tombstone and verified
   to be the very entry held. A replacement is put back, never removed, and the cleanup fails.
   A removed directory is confirmed gone through its handle. The one POSIX residue, between the
   tombstone's verification and its `rmdir`, is detected and reported as uncertain, never as
   success. A failed sample's original diagnostic always comes first, and the cleanup note is
   capped at 1 KiB inside the 4-KiB detail. The full note goes to stderr, capped at 16 KiB.
   Truncated stdout beyond the 64-KiB control record is an `oracle_mismatch`; stderr beyond the
   log cap is counted, not judged. The Python direct-child runner is removed.
9. **Capabilities (`cli.py capabilities`, `af.perf-capabilities/1`).** The report gives actual
   Git, rustc and Docker identities, with the reason for any that is missing; trusted local
   capability (macOS only, discovery and nonspawning library cases); Linux container capability
   (a Linux runtime with cgroup v2 and a verified scratch); and each registry environment with
   its reasons. Nothing is installed, configured or granted, and nothing falls back to a weaker
   mode. Every environment stays unavailable for runs until p3b.

## Consequences

- No host path that candidate code can replace is removed recursively for a Linux sample. On
  macOS, explicit trust replaces a claim of exclusive ownership that POSIX cannot back. The
  tests that pinned foreign deletion are replaced by tests that a replacement survives and the
  sample fails. An empty 0700 directory swapped in before the handle exists is documented as
  outside the boundary and is not presented as detected.
- Validated on the development host: the namespace tests pass on Linux Python 3.9.25 and
  3.11.17 and on macOS. A static Linux arm64 supervisor running as PID 1 in Docker Desktop
  (Linux 6.10, cgroup v2) killed and verified `setsid` and double-fork escapees, a held-pipe
  escape, a timed-out sleeper and a 5-MB flood, forwarded `docker kill -s TERM` to the child,
  and cleared its scratch. That was a 256-MiB, 64-PID tmpfs functional check, not the declared
  profile or a measurement. The same backend offers no disk-bounded scratch, so its Linux
  container capability is reported unavailable.
- Host builds are isolated by their environment and their private repository, not by the
  filesystem. A candidate build script still runs as the operator's user, so the credential-free
  build job of the design is the boundary in CI. Container builds are unavailable until a
  recipe pins its image; such a recipe is refused, not built on the host.
- `run.json`, `samples.jsonl`, `shard.json` and the verdict are unchanged. `builds.json` is
  `/3`, and the committed synthetic comparison fixtures were regenerated with it.
- Running the supervisor's child through `posix_spawn` means `SampleSpec` has no working
  directory. A one-sample supervisor enters the requested directory before it runs the sample.
