# ADR-0201: Key warm check layers by content, lock them, split the build deadline, and seal without generated files

**Status:** accepted (2026-10-23). Package P10 of the GA polish campaign. Amends
[ADR-0131](0131-warm-task-checks-through-a-toolchain-keyed-bounded-cache.md) (the warm layer's
lock wait, a third kind and the evidence it records) and supersedes the "every source is stamped
now" rule of
[ADR-0161](0161-refresh-native-check-source-mtimes-before-reusing-cargo-target.md). Keeps
[ADR-0144](0144-hold-afs-disk-use-to-a-machine-budget.md) (the Storage Budget) and
[ADR-0199](0199-rerun-a-failed-test-alone-report-the-gate-and-retry-a-failed-gate-on-the-same-candidate.md)
(the check report a gate writes to `$AF_CHECK_REPORT`).

## Context

Issue #266 recorded 35 gate runs that failed before any test with ``unresolved import
`review_core::…` ``. The warm layer of ADR-0131 is keyed by project and toolchain, not by the
tree it built, and materialized sources carry the fixed modification times of their CAS blobs, so
Cargo took a crate another Snapshot compiled as fresh. ADR-0161 stamped every source now, which
is correct but rebuilds every workspace crate on every check. Operators worked around both with a
private `XDG_CACHE_HOME` per branch, `rm -rf …/task-build-cache` before every run, and `touch` on
sources; on one VM the issue cost seven Tasks and was fixed by hand outside af.

Four more faults sat beside it. A check that found the layer busy waited 60 seconds and then
built cold, so concurrent sessions rebuilt the shared layer while a gate ran. The 3,600-second
`check_wall_ms` counted the cold build, so a layer evicted mid-gate left no time for the tests
(six runs recorded `check kernel: not_run in 3592567 ms`). The `markdownlint` check downloaded
`markdownlint-cli2` from npm in every gate (6–450 s; a DNS blip lost a verification, issue #230).
And a sealed candidate picked up `__pycache__/` and `*.pyc` from the Worker's own test runs, while
the sealer treated anything that was not a directory or a link as a file, so a FIFO hung it (issue
#160).

## Considered options

1. **Key the target by the whole Snapshot.** Rejected, as in ADR-0161: a new multi-gigabyte
   target per candidate, and no reuse of the crates a change did not touch.
2. **Stamp every source now (ADR-0161).** Correct, kept as the fallback, but it rebuilds every
   workspace crate.
3. **Record the content each crate was built from and stamp only what differs (chosen).** The
   difficulty ADR-0161 named — a failed or killed check would leave the record describing a build
   that never finished — is met by writing the record *before* the build, below.
4. **Lock with a 60-second wait (ADR-0131).** Rejected: a busy layer meant a cold rebuild inside
   a running gate. A check now waits for the layer until its deadline, or, opted in, builds in a
   private copy.
5. **One deadline for build and tests.** Rejected for the reason above; a second deadline needs
   the gate to say where its build ends, which the ADR-0199 report already does.
6. **Vendor the markdownlint tarball.** Rejected: dependencies would have to be vendored too. The
   npm cache is filled once and verified instead.
7. **Read only `.gitignore` while sealing.** Rejected: a project that commits no `.gitignore` for
   Python bytecode would still deliver it; the kernel adds a short default list a project can
   negate.

## Decision

1. **Content keys the build, not modification times.** A Snapshot's files fall into source
   groups: each Cargo package directory owns the files below it that no deeper package owns, and
   the files no package holds form the root group. Each group has a digest of its paths, kinds
   and content IDs. Beside each toolchain key the kernel keeps `<project>/<toolchain>.sources`,
   `af.warm-sources/1`: per group, the digest the key last built and the time the kernel gave that
   group's files. Before a check, under the key's lock, the record is removed; a group whose
   digest matches gets its recorded time back, so Cargo keeps what it built from that content;
   every other group (changed, new, or any group of a key with no record) is stamped now, after
   everything in the target, as ADR-0161 does, so Cargo rebuilds exactly those and what depends
   on them. The new record is written before the check starts. Whatever the check then builds
   into the key — passing, failing or killed — is built from the recorded content after the
   recorded time, so the record never claims a build it does not describe. A record that cannot
   be read or written leaves the key without one, which costs a rebuild and nothing else. A
   private copy of a busy layer, and a future-dated target, have no record: every group is
   stamped now.
2. **A held layer is waited for, named, and never evicted.** A check that takes a key writes
   `<project>/<toolchain>.holder`, `af.warm-holder/1` (pid, host, time, `af task run`), and removes
   it before it releases the lock. A check that finds the key held waits until its deadline and
   prints, every 60 seconds, `waiting for warm layer <key> held by af task run <pid> since <time>
   on <host>`. With `[warm] clone_when_busy = true` it does not wait: it copies the layer's
   directories into its runtime directory — `clonefile` on APFS, a reflink on btrfs and XFS, a
   plain copy otherwise, modification times kept, links and special files left out — and builds
   there, leaving the layer untouched. The Storage Budget reads a key's lock at inventory and
   takes it, without waiting, before evicting; a held key is refused and left in place, and an
   evicted one leaves `<project>/<toolchain>.evicted`, `af.warm-eviction/1`.
3. **The build has its own deadline.** `[warm] build_wall_ms` (at most 3,600,000; this repository
   declares 2,400,000) bounds a check's build phase, from its start until it reports a `build`
   step through `$AF_CHECK_REPORT` (`scripts/ci-step.py --stage build`); `check_wall_ms` (or
   `check_process_wall_ms`) then bounds its tests, from the build's end. A check that reported
   steps but no build step when its build deadline passed is ended there; one that reports no step
   at all has the two deadlines together as one. The check Attempt's wall is `check_wall_ms` plus
   `build_wall_ms`. A policy that declares no `build_wall_ms` keeps one deadline for the whole
   check, as before: a default would lengthen every warm check Attempt and refuse every Task file
   whose verification reserve was sized for `check_wall_ms` alone, so the split is opted into. A
   key that leaves its name while a check holds it is evicted under it: the monitor records the
   phase, the time and the cause (`storage sweep` when the sweep's eviction record is newer than
   the check, `another process` otherwise), and a check that did not pass gives that as its reason,
   `layer evicted during build by storage sweep at <time>`, ahead of what followed from it.
4. **An offline check needs no network.** A check declared `offline = true` gets
   `npm_config_offline=true`, every proxy variable pointed at the closed port
   `http://127.0.0.1:9`, and `CARGO_NET_OFFLINE=true`; it needs `[warm] build_cache` to declare
   `npm_cache`, a third kind bound as `npm_config_cache`. Its `npm_packages` pin each package as an
   exact `name@version` and the SHA-256 of its registry tarball. Before every run the kernel finds
   each tarball in npm's cache by its index key and SHA-512 integrity and compares its SHA-256 with
   the pin; a pin the cache lacks is filled once, by `npm exec --yes --ignore-scripts
   --package=<spec> -- true` with the network still allowed, and verified then; a mismatch refuses
   the check, naming the package and the pin. npm's install tree (`_npx`, which holds links) is
   removed from the layer after every check, before the layer is judged. This repository's
   `markdownlint` check is offline on `markdownlint-cli2@0.22.1`.
5. **Sealing leaves generated files out and never reads a special file.** A Worker's candidate
   leaves out what it added that the tree's ignore rules exclude: the kernel defaults
   `__pycache__/`, `*.pyc`, `/target/` and `node_modules/`, then every `.gitignore` (deeper files
   later), then `.af/seal-ignore`, matched as git matches them. A `!` rule stages a path back; a
   path the parent Snapshot held is never left out. What was left out is recorded as
   `af.seal-ignored/1` (`ignored[]`, at most 1,000 paths, and their count), referenced from the
   candidate's envelope beside its Manifest, and said on stderr. A FIFO, a socket or a device file
   anywhere in a sealed tree is refused by name, never opened.
6. **Each warm check says what it found.** `af/TaskCheckLayer@1` is recorded beside the check
   receipt of a warm check node: per started check, the key, `held`, `waited`, `cloned`, `busy`,
   `unavailable` or `unresolved`, the time waited and the holder met, `warm` or `cold` at the
   start, the source groups stale and fresh, the build and test phases against their deadlines
   with the one that ended the check, and an eviction with its phase, cause and time. `af task
   show --checks` prints it under each check, and its `--json` document is now
   `af/task-checks@2`, whose checks carry `layer`; `af/task-checks@1` is no longer printed.

## Consequences

- A gate that changes one crate rebuilds that crate and its dependents, and a return to earlier
  content keeps no stale artifact (`a_warm_layer_builds_snapshot_b_after_a_with_only_the_changed_crates_stale`
  builds A, B and A again with real Cargo, and its control reproduces the #266 failure). The
  workarounds — a private cache per branch, removing the cache, touching sources — are no longer
  needed.
- Two gates of one project and toolchain run one after the other instead of the second building
  cold; with `clone_when_busy` they run side by side at the cost of a copy.
- This repository's check Attempt is 100 minutes (60 for the tests, 40 for the build). A Task
  file whose verification reserve is smaller is refused at planning with the smallest limits that
  fit; `af task plan --fit` raises them.
- The markdownlint gate needs the network once per machine and toolchain key, when its cache is
  filled; after that it reads only the cache. Its other npm dependencies are verified by npm's own
  lockfile integrity, not by the pin.
- A candidate no longer carries bytecode or build output a Worker's test run left; a Worker that
  means to deliver such a file adds a `!` rule for it.
- New shapes, each with a schema and a parity test: `af.warm-sources/1`, `af.warm-holder/1`,
  `af.warm-eviction/1`, `af/TaskCheckLayer@1`, `af.seal-ignored/1`, `af/task-checks@2`.
  `af.code-task-policy/1` gains optional fields only (`[warm] clone_when_busy`, `build_wall_ms`,
  the `npm_cache` kind, a check's `offline` and `npm_packages`), so a policy without them is
  captured byte-identically; its schema also gains the `rust_toolchain` table the typed policy
  already accepted.
- `trusted_local` is still not isolation: a check can write the sources record or the holder
  record, as it can write anything the user owns. As ADR-0131 says, the records make the layer
  honest as evidence of what the kernel did, not proof against a check acting on the host.
