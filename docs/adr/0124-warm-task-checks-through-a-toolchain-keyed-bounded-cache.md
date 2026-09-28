# ADR-0124: Warm Task checks through a toolchain-keyed, bounded, machine-local cache

Status: accepted, 2026-09-27; amended the same day by package R1's follow-up (Task
`research-r1c`, see [Amendment](#amendment-rustup-home-cargo_home-and-fail-closed-bounds)), and
on 2026-09-28 by [ADR-0128](0128-collect-finished-tasks-behind-a-tombstone-and-a-reachability-sweep.md)
(package R5, see [Two bounds](#amendment-two-bounds)).
Supersedes in part
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
  `build_cache = ["cargo_target"]`, the only kind installed at first (the amendment below adds
  `cargo_home`); `caches = ["cargo"]`, naming Cache
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
  - the check's fixed `PATH`, `LC_ALL` and `TZ`, and, since the amendment, `RUSTUP_HOME`.

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
  After the check, a directory above the bound is removed, so the next check runs cold; since the
  amendment that measurement comes before the result is accepted and fails the check. A
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

## Amendment: rustup home, `cargo_home` and fail-closed bounds

The verification Task of R1 recorded `cargo_target cold toolchain_unresolved` on this machine: the
probe's `rustc -vV` exceeded its 30-second bound. A check receives a fresh `HOME`. A rustup proxy
`rustc` or `cargo` that finds no rustup home below that `HOME` syncs the channel and installs the
pinned toolchain there before answering. So the probe timed out. Every Task check also
downloaded a toolchain, and the whole crate registry, into a directory discarded after it. The
reviewers found five more defects, each closed below.

Options considered for the toolchain:

- **Raise the probe's bound.** Rejected: the probe would answer, but every check would still
  download a toolchain and the registry, and the key would be computed from a toolchain the
  check just installed.
- **Make the rustup home a warm kind.** Rejected: the installed toolchains are the machine's,
  not candidate-built state. Bounding or removing them would delete the operator's toolchains,
  and a check could write a toolchain that the next Task then runs.
- **Pass the kernel's `HOME` to the check.** Rejected: it hands a candidate the operator's whole
  home, including credentials, which the runner's allowlist exists to withhold.
- **Pass only the rustup home, and forbid installing (chosen).**

Decisions, each extending the rule above that it names:

- **Rustup home.** Under `[warm]`, and only then, the check and its toolchain probe receive
  `RUSTUP_HOME` and `RUSTUP_AUTO_INSTALL=0`. `RUSTUP_HOME` is the kernel process's own, made
  absolute, else `$HOME/.rustup` of the kernel's `HOME` when that directory exists, else it is
  unset. The installed toolchain answers at once. A toolchain the machine lacks fails the probe
  (`toolchain_unresolved`) and is never downloaded. `RUSTUP_HOME` joins the key's fixed
  environment. The kernel never creates, writes, bounds or removes the rustup home. Where the
  value came from is recorded on each warm check as `rustup_home`: `kernel_environment`,
  `kernel_home`, `unset_no_home` or `unset_not_installed`.
- **`cargo_home`.** `build_cache` admits a second kind, `cargo_home`, the directory
  `<project>/<toolchain>/cargo_home` bound as `CARGO_HOME`. It is Cargo's registry and git
  cache. It has its own lock and the same validation, removal and observation rules as
  `cargo_target`. Locks are always taken in one kind order, so two checks never hold one kind
  each while waiting for the other's. `max_bytes` bounds the kinds of one toolchain key
  together: before, during and after the check, their combined bytes are compared with it, and
  crossing it removes every one of them. A `cargo_home` holding `credentials.toml`, or Cargo's
  older `credentials`, is suspect and removed before reuse. A declared `caches = ["cargo"]` Cache
  Snapshot takes precedence for `CARGO_HOME`. The two are never bound at once, and the
  `cargo_home` kind records the ineligible `cargo_home:superseded`. The warm kinds are their own
  closed vocabulary, not a Gate's `build_caches` of ADR-0108.
- **Bounds, after every execution.** The five-second monitor stays for long checks. Once any
  monitored check has ended, and before its result is accepted, the directories are measured
  again. Above the bound the check is `failed` with `warm_cache_bound_exceeded` and the
  directories are removed under their locks. A check that writes past the bound between two
  samples therefore fails too. The eviction stays on each kind's one observation as
  `evicted_bytes`.
- **Traversal fails closed.** The byte count and the suspect walk open each directory relative
  to its parent's descriptor with `O_NOFOLLOW` and inspect each entry with
  `fstatat(AT_SYMLINK_NOFOLLOW)`. Only an entry that vanished during the walk is skipped. Any
  other failure, such as an unreadable subtree, is never skipped: before reuse it makes the
  directory suspect, so the directory is removed. After a check it is a directory above every
  bound, so the check that left it fails through the bound path.
- **Measured after validation.** `ensure()` reports whether it reused, created or discarded the
  directory, and bytes are measured only after it. A discarded directory is recreated empty
  and recorded as an eligible observation with `bytes_available = 0`, a cold check, never with
  the discarded directory's old bytes.
- **Every check keeps its evidence and its name.** Every check of a warm Task has exactly one
  `af/TaskRuntimeEvidence@1` group. The group carries a `check` binding with the check's name,
  its final outcome and its `rustup_home`, and one observation per declared kind, whether or not
  the check started. A check skipped because the Attempt's deadline ran out before or during
  preparation records `<kind>:deadline_exhausted` for every kind. A check that did not run
  because a declared Cache Snapshot was refused records the refused kind as `unavailable` and
  every other kind it had prepared as `<kind>:cache_refused`. `af task show` prints named lines
  from that binding, `check <name>: <outcome> in <ms> ms, …` or
  `check <name>: not_run, never started, …`, and never an unnamed line. The binding is optional
  and absent without `[warm]`, so those records keep their bytes.
- **Proof of the unchanged path.** A checked-in golden, recorded by a kernel built from the
  Snapshot this amendment started from, holds the revision, plan, run authority, `af task show
  --json` inspection, check receipt, check result, delivery receipt and delivered tree of a Task
  without `[warm]`. The golden is normalized only where two independent runs differ: deadlines,
  Attempt identities, times, fixture paths, and the engine digest of the running `af`
  executable. The current kernel must reproduce it. A second fixture shows that no path or
  content of a populated cache directory appears in the implementer Worker's sandbox manifest,
  the sealed candidate manifest, the derived Snapshot or the delivered tree.

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
  check Attempt with no cache observation or check binding, no cache line in `af task show`, no
  rustup variables in the check's environment.
- A machine without a rustup home, or without the pinned toolchain, runs every warm check cold
  and says so. It no longer downloads a toolchain per check.

### After the second review

Three rules were added when the follow-up Task's reviewers read the amendment:

- **One key, one holder.** A check takes an exclusive lock over the whole toolchain key
  (`<project>/<toolchain>/warm.lock`) before any kind's lock and holds it until its removal step
  has ended, so two checks of one project and toolchain never overlap whatever kinds each
  declares; a check that cannot take it within the wait runs cold and records `busy`. The bound
  is measured over the whole key directory, held kinds or not.
- **Suspect after the check, not only before it.** Once a check ended, every directory it used
  is judged as `ensure` judges it before reuse: a link, a special file, another user's entry, a
  forbidden name such as a Cargo `credentials.toml`, or a root that is no longer a private real
  directory makes it suspect. The check fails with reason `warm_cache_suspect`, and every
  directory of the key is removed under the lock before it is released.
- **A swapped root is uninspectable.** A warm root that is a link or a file once the check ended
  cannot be counted; it is above every bound, the check fails through the bound path, and the
  entry is removed.

### After the second verification

- **Descriptors, not paths.** Every operation below the toolchain key goes through the key
  directory's open descriptor — `mkdirat`, `openat(O_NOFOLLOW)`, `fstatat(AT_SYMLINK_NOFOLLOW)`,
  `unlinkat` — never through a path. A check that renames the key's parent and plants a link in
  its place changes nothing the kernel resolves; cleanup removes what the kernel held, and only
  that.
- **The whole key, before and after.** Under the key lock the bound is measured over every entry
  below the key, held by this check or left by another policy's; a key over its bound or
  uninspectable is emptied of every kind before this check binds anything, and after a check that
  ended over the bound or suspect every kind below the key is removed, not only the ones it held.
- **The cause travels with the eviction.** An observation's `evicted_bytes` comes with
  `evicted_reason`, `bound_exceeded` or `suspect`, and `af task show` prints that reason.

### After the third verification

- **A closed kind set, and only the kernel's locks are exempt.** Only `cargo_target` and
  `cargo_home` are ever locked as kinds; below a key, only `warm.lock` and those two kinds' lock
  files are the kernel's, and they are truncated on every acquisition so a check cannot park bytes
  in them. Every other entry below the key — a kind directory, a link a check planted where a kind
  was, a file a check named to look like a lock — is counted against the bound and removed with the
  key.
- **A superseded kind still shares the key.** A policy whose every kind a Cache Snapshot supersedes
  still resolves the toolchain, takes the key lock, measures the key and empties an over-bound one;
  its observations carry the resolved toolchain like every other kind's.
- **Any excess fails the check.** A check whose used directory is suspect once it ended fails
  whether or not anything was left to remove — a check that deleted its own warm root is suspect
  too — and every declared kind's observation carries the eviction and its cause, with zero bytes
  when nothing was there.

### After the fourth verification

- **Locks are inodes, not names.** Only the lock files this holder opened — by the inode it holds
  — are exempt from the key's count and eviction. A file a check parks at a lock's name after
  unlinking it, or a kind lock nobody holds, is counted and removed like any other entry, and a
  held lock whose name no longer holds its inode makes the key suspect.
- **The key itself is judged.** Before the check is accepted the key directory must still be a
  private directory of this user and every held lock must be in place; a widened key is suspect,
  the check fails and the key is emptied. On acquisition a key that is not private is emptied
  before it is made private again, never repaired and reused.
- **Held is monitored.** Whenever a check holds the key — even when every declared kind is
  superseded and nothing is bound — the key is measured during the check, measured and judged after
  it, and every declared kind's observation carries an eviction and its cause.

### After the fifth verification

- **A held lock is one name, one inode, zero bytes.** Only the top-level entry at a held lock's
  exact name holding its exact inode is exempt from the key's count and eviction; a hard link to
  that inode anywhere else is counted like any file, and a held lock that has a second name or has
  grown makes the key suspect.
- **Eviction cannot be refused.** The key is made writable through its held descriptor before
  entries are removed, so a check that took write permission away keeps nothing; entries are
  addressed by their exact bytes, so a name that is not UTF-8 goes too; and an eviction that leaves
  any entry but this holder's locks behind is an error, never a reported success.
- **The cause on every observation.** When a check fails for excess, every declared observation
  of that check — a warm kind, a superseded kind, a Cache Snapshot — carries the eviction and its
  cause, with zero bytes where nothing was below the key for it.

### After the sixth verification

- **A lock is judged before it is written.** Acquisition opens the lock's name without following
  links and truncates nothing until the inode it opened is a plain, singly linked file of this
  user that still sits at that name; anything else is unlinked at the name, never written through,
  and a fresh file takes its place.
- **The key must stay at its name.** The project level is held open too, and a key that a check
  renamed and recreated is displaced: suspect, failed, and evicted through the descriptor of the
  directory the kernel actually held.
- **Eviction settles the held locks.** A sound held lock with bytes is emptied through a fresh
  descriptor to the same inode; a replaced or linked one is unlinked at its name; and eviction
  reports success only when nothing but sound, empty held locks remains.
- **Suspicion outranks the count.** When a check ended over the bound and suspect, the recorded
  cause is `suspect`.

### After the seventh verification

- **A lock's name is cleaned before it is opened, and the inode is judged again after the wait.**
  A link or a directory an interrupted check left at a lock's name is removed by name before
  `openat`, so recovery never stalls on `O_NOFOLLOW` refusing it; and a waiter that judged the
  inode sound before waiting on another holder judges it again — on the open descriptor and at
  the name — once it holds the lock, before anything is written through it.

### After the eighth verification

- **A held lock is exempt from the bound only while it is the empty file the kernel made.** One a
  check grows or links counts like anything else, so the running bound catches it, not only the
  judgement after the check.
- **Removal addresses every entry by its exact bytes**, never by a lossy spelling: a name that is
  not UTF-8 and the name its spelling collides with are two entries, and both go.
- **A directory's identity is its project, toolchain and kind alone**, never the lookup's outcome:
  a warm, a busy and a discarded observation of one directory join on one `source_digest`.
- **Recorded, not fixed:** a check that unlinks the lock it holds and plants another entry at the
  name lets the next check recover onto a fresh inode while the first still runs. That is the
  check acting against the cache it was trusted with, which is where these rules stop (below).

### Where these rules stop

`trusted_local` is not security isolation, and this cache does not claim to make it one. A
check runs as the user; it can rename or delete anything the user owns, including this cache's
parents and the Store. The rules above make the cache's own directory honest as *evidence*: a
check cannot make a warm gate report fewer bytes than it holds, cannot leave a credential, a link
or an unreadable subtree where the next gate would build on it, and cannot have the kernel write
through an inode it planted. They do not defend against a check that acts on the host outside
the key, and a review that treats that as a defect of this cache is reviewing the isolation
policy, not the cache: `require_container = true` refuses `[warm]` for that reason.

## Amendment: two bounds

[ADR-0128](0128-collect-finished-tasks-behind-a-tombstone-and-a-reachability-sweep.md) splits the
one byte bound above in two, because a single bound ended R2's implementation Attempt 61 s into
its gate for growth the candidate did not cause. Where this record says a check that grew the
directories past "the bound" fails, read `hard_max_bytes`:

- **`max_bytes` is the eviction bound.** Before a check, a key above it is removed and the check
  runs cold, unchanged. After a check, a key above it but not above `hard_max_bytes` is removed
  under the key lock; the check's own result stands, and only the next check finds the
  directory gone.
- **`hard_max_bytes` is the only bound that ends a running check**: the monitor samples against
  it, and the measurement when the check ends fails the check above it, with
  `warm_cache_bound_exceeded` as before. It defaults to twice `max_bytes`, at most 32 GiB, and a
  declared value must lie between `max_bytes` and 32 GiB.
- **The observation says which acted.** `TaskCacheObservationV1.bound` is `max_bytes` for a
  removal before a check or an eviction whose check's result stood, and `hard_max_bytes` for a
  check the bound ended; it is absent otherwise and never beside a `suspect` eviction.

Suspicion, an uninspectable key and every other rule above are unchanged.
