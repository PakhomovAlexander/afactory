# ADR-0123: Warm Task checks through a toolchain-keyed, bounded, machine-local cache

Status: accepted, 2026-09-27. Supersedes in part
[ADR-0108](0108-carry-gate-build-caches-as-explicitly-unsafe-warm-layers.md): the one-Round
scope of a candidate-built build cache, for Task checks only.

Implements package R1 of [`docs/design/research-pipelines.md`](../design/research-pipelines.md)
under that plan's §2, on top of ADR-0108 (candidate-built build caches are explicitly unsafe),
[ADR-0036](0036-resolve-gate-caches-through-machine-local-bounded-policy.md) (Cache Snapshots
resolved through machine-local policy) and
[ADR-0026](0026-share-process-supervision-through-a-leaf-crate.md) (one supervised kill path).

## Context

Every Task check builds cold. `code.rs` gives each check a fresh `HOME`, `XDG_CACHE_HOME` and
`CARGO_TARGET_DIR` inside a private runtime directory, so `scripts/verify.sh`'s external target
directory is never warm inside a Task. The design records a median check stage of 15.6 minutes
and 8.6 hours of Gate time, most of it spent compiling one unchanged workspace.

ADR-0108 already admits candidate-built build output as an explicitly unsafe warm layer, but only
within one Round: a Gate captures its target into the CAS and Workers of the same Round clone it.
Nothing carries a build from one check to the next. A check is also a different consumer from a
Worker. It is a kernel-run command from committed policy, not a model, and its sandbox is
read-only, so a build directory it reuses never needs to travel through a Worker sandbox.

## Considered options

- **Capture the check's target into the CAS, as ADR-0108 does, and clone it into the next
  check.** Rejected: a Rust target is gigabytes of churning files. Capturing and cloning it
  costs roughly what the build saves. It also grows the Store with bytes no record needs, and
  the design already counts 22 GB of Store with no collection.
- **Point every check at one shared host directory without a key.** Rejected: a compiler or
  toolchain change would reuse output built by another toolchain. Two concurrent checks would
  write one directory. Nothing would bound the directory, and nothing would say whether a check
  was warm.
- **Clone the previous check's output per Attempt.** Rejected for the same cost as the CAS
  option, and it needs an ordering between Attempts that the runtime does not have.
- **A keyed, locked, bounded host directory that only the check runner opens (chosen).**

## Decision

- **Admission.** `af.code-task-policy/1` gains an optional `[warm]` table:
  `build_cache = ["cargo_target"]`, the only kind installed; `caches = ["cargo"]`, naming Cache
  Snapshot kinds; and `max_bytes`, which defaults to 8 GiB and may be at most 32 GiB. Loading
  the policy refuses a `[warm]` table together with `require_container = true`, and the refusal
  names both. Loading happens at Task capture, before any Worker or Provider admission. A
  Warm Check Cache is therefore `trusted_local` only, and only when committed project authority
  declares it. `schemas/code-task-policy-v1.json` states the shape. A policy without `[warm]` is
  captured byte-identically to before, so its revision and plan keep their identities.
- **Location and key.** A declared `cargo_target` points the check's `CARGO_TARGET_DIR` at
  `$XDG_CACHE_HOME/af/task-build-cache/<project>/<toolchain>/cargo_target`. The kernel resolves
  that path from its own environment; each check still gets a fresh `HOME` and
  `XDG_CACHE_HOME`. `<project>` is a domain-separated digest of the source origin's repository
  identity (its root commit set). `<toolchain>` is a domain-separated digest of five inputs:
  - the Snapshot's `rust-toolchain.toml` or `rust-toolchain` bytes, or the literal `none`;
  - the complete `rustc -vV` output;
  - the complete `cargo -vV` output;
  - the host triple `rustc` reports;
  - the check's fixed `PATH`, `LC_ALL` and `TZ`.

  Both version commands run once per check Attempt, before its first check. They run in the
  check's working directory, under the check's exact environment, with a 30-second supervised
  bound. If either fails, or no host triple is reported, the check runs cold with
  `cargo_target:toolchain_unresolved`. No component of the path is text that a repository
  controls.
- **Cache Snapshots.** A declared `caches` kind resolves through the machine's cache policy, as
  a Gate's `[gate] caches` do. It is materialized below the check's private runtime directory,
  never into the read-only source tree. It binds `CARGO_HOME` and `CARGO_NET_OFFLINE=true`. An
  unresolvable kind fails its check as `not_run` before dispatch, as a Gate does. That check
  records `cargo:unavailable`.
- **Exclusion.** Each directory has one exclusive advisory lock, on a sibling `<kind>.lock` file,
  held for the whole check. A check that cannot take the lock within 60 seconds runs cold in
  its private runtime directory and records `cargo_target:busy`.
- **Bounds, three times.** Before the check, a directory already above `max_bytes` is removed and
  the check runs cold, recording `cargo_target:bound_exceeded`. During the check, a monitor
  samples the directory's size every five seconds. Above the bound it ends the check's process
  group through the check runner's supervised cancellation. The check is recorded `failed` with
  reason `warm_cache_bound_exceeded`, and the directory is removed before the lock is released.
  After the check, a directory above the bound is removed, so the next check runs cold. A
  removal during or after the check is recorded on the kind's one observation as
  `evicted_bytes`, so every declared kind yields exactly one observation per check: the bytes
  available before the check and, when it happened, the eviction after it. Only a removal
  *before* the check is an ineligible `cargo_target:bound_exceeded` observation, because that
  check never had the directory.
- **Invalidation.** A directory is removed and never repaired. This happens when it is over its
  bound, and when it is suspect: a link, not a directory, owned by another user, or not mode
  `0700`. A directory under a different `<toolchain>` key is never read, because a changed
  toolchain names a new directory. The kernel creates every level it owns with mode `0700`.
  Size is counted without following links.
- **Isolation.** The directory is outside every sandbox. Only the kernel's check runner opens it.
  It is never mounted, cloned or copied into a Worker sandbox, and candidate capture never reads
  it. No Snapshot, candidate or delivered tree can contain its bytes. It holds no credential: the
  check environment is still the runner's allowlist.
- **Evidence.** Each warm check settles its own `af/TaskRuntimeEvidence@1`. The record holds the
  check's `Check` span and one `TaskCacheObservationV1` per declared kind. An observation records
  `kind`, `eligible`, `toolchain_id`, `bytes_available` before the check, `lookup_ms` and
  `materialization_ms`. Its `source_digest` is the path-free identity of the directory: project,
  toolchain and kind. An eligible observation with `bytes_available = 0` is a cold check. An
  ineligible one names its reason in `kind` as `<kind>:<reason>`, and the evidence schema now
  admits that one suffix. `af/task-inspection@11` carries the records in `runtime_observations`.
  `af task show` prints one line per warm check with its elapsed time and `<kind> warm <bytes>`
  or `<kind> cold <reason>`. A check is warm or cold only in this evidence. Its
  `CheckResult@1` bytes, receipt outcome and every Snapshot are the same either way.

## Consequences

- A second check on one project and toolchain starts from the first one's build. How much faster
  that is gets recorded as benchmark Evidence in the design's §6, not asserted by a fixture.
- A `trusted_local` check can now leave state that a later check reads. Candidate code in one
  Task can therefore influence the build of a later Task on the same machine. That is the unsafe
  part ADR-0108 names, and this ADR widens it from one Round to the machine for checks alone. The
  bound, the key, the lock and removal on suspicion limit it; `require_container = true`
  refuses it.
- The disk the cache uses is bounded per project, toolchain and kind. Old toolchain directories
  stay until an operator removes them. Removing `$XDG_CACHE_HOME/af/task-build-cache` is always
  safe.
- `scripts/verify.sh` honours a `CARGO_TARGET_DIR` that is already set, so the kernel's own gate
  builds into whatever directory its check was given.
- A policy without `[warm]` is unchanged: same captured policy bytes, one runtime record per
  check Attempt with no cache observation, no cache line in `af task show`.
