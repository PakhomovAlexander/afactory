# ADR-0144: Hold af's disk use to a machine budget

**Status:** accepted (2026-10-07). Amends
[ADR-0131](0131-warm-task-checks-through-a-toolchain-keyed-bounded-cache.md): old toolchain keys
are evicted by the Storage Budget instead of being left to the operator, and the key no longer
depends on a check's runtime path. Amends
[ADR-0135](0135-collect-finished-tasks-behind-a-tombstone-and-a-reachability-sweep.md):
collection also runs by itself after every run. Amends
[ADR-0140](0140-run-a-declared-check-through-a-gate-pull-request.md): when a Task finishes, af
closes its draft gate pull request and deletes its two `af-gate/` branches. The plan is
[`docs/design/disk-budget.md`](../design/disk-budget.md).

## Context

af kept nearly every byte it wrote, and nothing bounded the total. On the maintainer's machine
the Warm Check Cache made a new 4.7 GiB key for every check Attempt: a prepared native toolchain
put the check's fresh temporary directory into `PATH` and `RUSTUP_HOME`, and the key hashed both,
so 13 of 13 kernel checks ran cold and 26 keys held 117 GiB in two days. `max_bytes` bounded one
key, not the number of keys, the review campaign Stores (17 GiB), the Task Stores or the installed
versions; `af task gc`, `af review gc` and `af self prune` ran only by hand, and 14 of 19 Task
Stores could not be opened by the running release, so even `af task gc` could not reach them.
Checks had no writable place af owned: review gate checks ran with no `HOME`, so this repository's
`scripts/verify.sh` built a 16.7 GiB target under `/tmp/.cache`. Every Claude Worker Attempt and
admission probe left a `projects/<slug>` directory in the operator's Claude history, and every
remote check left two branches and a draft pull request open.

## Considered options

1. **A larger per-key bound.** Rejected: it does not bound the number of keys, which is what
   filled the disk.
2. **Collection on every `af` command.** Rejected: it would slow every command. Collection runs at
   the end of runs, before a new warm key, at the free-disk floor, and on request.
3. **A private `CLAUDE_CONFIG_DIR` per Worker.** Rejected: the login lives with the config
   directory, so a throwaway one would not be signed in.
4. **Leaving gate pull requests for the operator.** Rejected: that is the behaviour that left
   #179, #180, #185 and #187 open.
5. **Age-based collection after every run by default** (`auto_gc = true`). Rejected on
   2026-10-07: a test run of this package's own branch, whose tests reached a developer's real
   state directories, collected that developer's review history by age alone: 62 campaigns and 12
   Task Stores the machine still had room for. Age is a reason to delete only when the operator
   asks for it; the budget is the reason by default.
6. **One machine-wide budget over entries af evicts whole, least recently used first, enforced
   after every run, plus a free-disk floor, collection by age on request (`af storage prune
   --apply`, or `auto_gc = true`), af-owned check directories and removal of what Workers and
   gates leave in other tools.** Chosen.

## Decision

1. **A stable warm key.** `toolchain_identity` replaces the check's runtime path, as given and as
   resolved, with a fixed token in every environment value it hashes, and hashes a prepared
   native toolchain's verified content digest; the domain is `af.task-build-cache.toolchain/2`,
   so every older key goes stale once and the budget evicts it. A check Attempt resolves one key
   per distinct hashed environment, so two checks with different environments get different
   keys. Two checks with the same toolchain, environment and repository share one key across
   Attempts, Tasks and processes.
2. **`[storage]`, machine layers only.** A typed `StoragePolicy` with built-in defaults:
   `max_bytes = "20GiB"`, `min_free_bytes = "10GiB"`, `auto_gc = false`, `keep_days = 14`,
   `keep_tasks = 20`, `keep_campaigns = 20`, `keep_worker_transcripts = false`,
   `keep_gate_pull_requests = false`; byte counts are integers or B, KiB, MiB, GiB, TiB.
   `AF_STORAGE__<KEY>` overrides it. A directory, project or local layer that carries
   `[storage]` is reported as ignored, exactly as `[self]` is: a repository cannot steer what af
   keeps or removes on a machine.
3. **Entries.** The budget counts warm toolchain keys (last use: `warm.lock`, touched on every
   acquire; size: the record a warm check writes beside the key when it finishes, else
   measured), warm Workspaces (held by a shared `workspace.lock` while a run clones from them),
   review campaign Stores, finished Tasks of every Task Store (collected one at a time through the
   Store's own collection, so each keeps its tombstone), Stores this release cannot replay (removed
   whole), and installed versions. Sizes are allocated bytes, links not followed; a Store that
   cannot be measured whole is a problem every sweep reports, never counted as empty, and a Task
   Store whose Tasks cannot be listed is counted whole, kept, and reported; the files a
   reader creates (`-shm`, an empty `-wal`) never count as use. Inventory records each
   directory's identity (device and inode). Every removal the sweep, `af storage prune` and
   `af self uninstall --purge` make opens its target from a trusted anchor — the configured root
   as given (`$XDG_CACHE_HOME/af/…`, `$XDG_STATE_HOME/af/…`, `$XDG_DATA_HOME/af/versions`, a
   Claude config directory's `projects`), or `/` for a Store registered elsewhere — through
   directory descriptors, `O_NOFOLLOW | O_DIRECTORY` on every component below the anchor, and
   removes the tree through the descriptor it reached, never through a path. A symlink on the way
   from the anchor to the target, or a directory that is not the one inventory measured, makes
   that removal fail and be reported; it is never followed. Every removal requires that measured
   identity: a warm key is compared with the key directory its lock holds open, and a directory
   whose identity was never measured is left. Inside the removed tree a symlink is unlinked as
   the entry it is, never followed, so a Store or campaign that holds a link can still go. A
   removal first claims the measured directory under a private name (`.af-removing-<pid>-…`) in
   its parent, checks its identity there, empties it through its descriptor, checks the claim
   again and unlinks it. What fails part way stays under its claim (renaming it back could
   replace whatever took the name since), and a claim whose process died is finished by the next
   sweep, in af's roots, the temporary directory and every registered Claude config directory,
   and reported like any removal (kind `claim`, rule `recovery`), counted in the sweep's totals.
   The same claimed, identity-bound removal serves a check runtime when its check ends, the crash
   sweep of `af-sandbox-*` and `af-check-*` directories, and a Claude Attempt's or probe's
   project directory, each measured when it is chosen. A campaign or Workspace is held by an
   exclusive lock from that look until it is gone, so no run starts on it in between. A
   registered Store is reached from `/` one component at a time without following a link, its
   identity recorded; collection and gate cleanup check that identity again before they open it.
   A root that exists but cannot be listed is a problem the inventory reports, never an empty
   one. POSIX has no unlink by descriptor, so a
   directory put at that unguessable private name between the last check and the unlink could
   still go; only an empty one can, and that residual risk is accepted.
4. **One sweep.** Collection first, when asked: gate leftovers of finished Tasks, finished Tasks
   beyond the newest `keep_tasks` of each Store, campaigns beyond the newest `keep_campaigns`, and
   unreadable Stores, each idle at least `keep_days`. A finished Task whose remote checks pushed
   branches or opened a pull request is collected — by the sweep, its budget step and `af task gc
   --apply` alike — only once a gate cleanup for it is recorded done: collection runs the cleanup
   first, and a Task whose cleanup does not end done stays for the next sweep, unless
   `keep_gate_pull_requests` keeps everything. Then the budget: while af holds more than
   `max_bytes`, the least recently used entry goes, and what each removal freed is measured (a
   Task's collection can free more than its own bytes), so the step stops as soon as af fits. Never an entry in use (a held lock, a live
   writer lease, a running `af review run`, the default or a pinned version, the running binary)
   and never one used within the last hour. An installed version's protection is read again
   immediately before its removal, under the versions lock `af self` holds while it changes the
   default or records a pin; one that became protected since inventory is kept and reported. It
   stops when the total fits or nothing else may go, and says which. Every removal is reported on
   stderr, and the sweep that ends `af task run` records its removals, failures, stop reason and
   totals on the Task that run executed — a Task that sweep never collects or evicts, so the
   record always has its Task; a later sweep takes it under the same rules as any other — as one
   `storage_sweep` transition carrying
   `af/TaskStorageSweep@1` inline (at most 256 removals and 64 failures listed, the rest counted;
   a sweep that removed nothing and failed nothing records nothing), which `af task show` prints
   as one line. Triggers: the end of every `af task run`, `af task start --execute` and `af
   review run` whatever the outcome, a run refused before it started work included (the budget
   step and the retry of pending gate cleanups; collection by age too only when `auto_gc` is on,
   which it is not by default; its failure is a warning) — an interrupted
   run is not an outcome: after `Ctrl-C` af stops promptly and leaves collection to the next run
   or `af storage prune --apply`, because a sweep can wait on GitHub for gate cleanups — a warm check
   about to create a key that does not exist (budget only), the free-disk floor, and `af storage
   prune --apply`. An unchanged Task Store whose last collection
   found nothing to do before a known time is not planned again until then.
5. **The free-disk floor.** Before a check, before each measurement repetition materializes its
   sandbox, and before a Worker Attempt or a provider probe starts (a probe outside a run, such
   as a standalone `af provider status`, has no budget installed and no floor), af reads the free
   bytes of every
   volume it works on: the ones holding its temporary directory and `$XDG_CACHE_HOME`. Below
   `min_free_bytes` on any measured volume — even when another could not be measured — or with a
   volume that cannot be measured at all, it sweeps (every time the floor is met, since what
   became evictable since the last one may restore the room; the same sweep as after a run: the
   budget, and collection by age only when `auto_gc` is on); still so, it refuses before anything
   is prepared for the work (a Task check, a review Gate or a post-apply check gets no sandbox, no
   cache and no runtime), and once more right before a check or a measurement's command starts,
   after what its preparation wrote: a check is
   `not_run` with reason `insufficient_disk: …`, a remote check is refused with the typed reason
   `insufficient_disk` before its private repository or any push, a measurement fails with the typed reason
   `insufficient_disk` without starting its command, and a Worker Attempt is released before it
   starts, charged nothing, and the run stops like an interrupted one: nothing is assembled or
   finished, and the Task stays resumable. A failed measurement never counts as room. The message
   names the free bytes (or the measurement error), the floor, `af storage` and the knob. When
   the `[storage]` policy is invalid or af's directories cannot be resolved, no floor can guard
   the work, so `af task run`, `af task start --execute`, `af review run` and `af provider
   doctor` refuse to start and say why; likewise a review run that cannot hold its campaign's
   run lock does not start, since a sweep could otherwise take its Store.
6. **The Store registry.** A Task or review Store af opens or creates outside its default roots
   (`--state`, `--state-root`) is recorded, best effort, in `$XDG_STATE_HOME/af/stores.toml`;
   the sweep visits it and drops entries whose Store is gone.
7. **`af storage`.** It prints, per kind, the entries, bytes and oldest and newest use, the
   unreadable Stores with their reason, the total against `max_bytes` and the free bytes against
   `min_free_bytes` (`--json`: `af/storage@1`). `af storage prune` previews the sweep with
   collection on; `--apply` performs it (`--json`: `af/storage-prune@1`). `af review gc` gains
   the liveness check it lacked: a campaign a running `af review run` holds, or whose Task holds a
   live writer lease, is listed `in use` and never removed.
8. **af-owned check directories.** Every check — a Task code check, cold or warm, and the local
   part of a remote one; a measurement repetition; a review gate check; a post-apply integration
   check — runs with `HOME`, `TMPDIR`, `AF_CHECK_SCRATCH` and `XDG_CACHE_HOME` below one directory
   `af-check-<pid>-<random>` in the temporary root, each created empty and private and removed
   without following a link when the check ends. The startup crash sweep removes those of dead
   processes, as it does `af-sandbox-*`. Review gate and integration checks also get the
   kernel's `RUSTUP_HOME` (host-local only) and `RUSTUP_AUTO_INSTALL=0`. A container check sees
   the same four variables pointed at `tmpfs` mounts inside the container, so the sandbox stays its
   only bind.
9. **What Workers and gates leave in other tools.** Every probe that launches a provider CLI runs
   in a fresh `af-sandbox-<pid>-<random>` directory af made for it, which the startup crash sweep
   removes if af dies. When a Claude Worker Attempt's or probe's process exits, whatever the
   outcome, af removes `<CLAUDE_CONFIG_DIR>/projects/<slug>` of its working directory through
   descriptors — only when that directory is an `af-sandbox-*` or `af-check-*` directory af made
   for that Attempt or probe, never another (`projects/-` included); `keep_worker_transcripts`
   keeps them. When a Task finishes, and before collection takes a Task, af cleans up its gate
   against the Task's recorded `RemoteCheckEvidenceV1`, through a mapping target that names the
   recorded repository. It closes a recorded pull request only while it is open, its base
   repository is the recorded one, its head and base are exactly this Task's
   `af-gate/<task-id>/head` and `af-gate/<task-id>/base`, and its head commit is the recorded
   head commit, read again immediately before it is closed. GitHub has no conditional close, so a
   pull request whose head moves in that last instant can still be closed; a close is reversible
   and deletes nothing, and the branches are protected as follows. It deletes a branch only while
   `ls-remote` on the push URL shows exactly the recorded commit for it (base: the base commit,
   head: the latest head commit), in one atomic push of the matching deletions that carries
   `--force-with-lease=<branch>:<recorded commit>` for each: the remote deletes a branch only
   while it still holds that commit, so a branch moved after the read-back is not deleted either,
   and neither is the other one. A lease on a deletion is a compare-and-delete, not a force: it
   overwrites nothing, and the gate never pushes `--force`, an unbound lease or a `+` refspec. It
   uses the gate's own `gh` and `git` and never names another ref. A branch or pull request that differs is left in place and named in the reason of
   a failed cleanup. The result is one `gate_cleanup` transition carrying `af/TaskGateCleanup@1`
   inline: done, or failed with its redacted reason. It never changes the Task's result; a failed
   one is retried by every applied sweep (after every run and by `af storage prune --apply`,
   whatever `auto_gc` says) while the mapping still names the repository;
   `keep_gate_pull_requests` keeps everything. `af self uninstall --purge`
   also removes the af-made project directories in every registered Claude config directory and
   every registered Store, and lists the gate leftovers it could not reach.

## Consequences

- af holds a bounded, visible amount of disk: `af storage` shows it, and nothing an operator
  depends on is removed while it is in use or within the hour.
- The first sweep after upgrading evicts every key of `af.task-build-cache.toolchain/1`.
- A check that wrote to `/tmp` or `~/.cache` keeps working but should write to
  `$AF_CHECK_SCRATCH` or `$TMPDIR`; a gate check no longer shares the operator's `HOME`, so
  Cargo's registry comes from a Cache Snapshot or the check's own `CARGO_HOME`.
- Below the floor af refuses work instead of filling the disk; the debug-only
  `AF_TEST_FREE_BYTES` fakes the free bytes for fixtures, as `AF_TEST_GC_STOP_AFTER_TOMBSTONES`
  does for collection.
- `TaskTransition@5` gains the `gate_cleanup` and `storage_sweep` changes and
  `af/task-inspection@11` the optional `gate_cleanups` and `storage_sweeps` lists; a release
  before this one cannot read a log that holds one. `af/Measurement@1` gains the failure reason
  `insufficient_disk`.
- Removals of a sweep are reported on stderr and in `af storage prune --json`, and the sweep that
  ends `af task run` records them as a `storage_sweep` observation of the Task that run
  executed.
- A Task whose gate cleanup keeps failing stays uncollected until one is done; a branch or pull
  request someone else moved is never closed or deleted by af, so such a Task's cleanup is
  finished by hand (`af task show` prints the commands) or by setting
  `keep_gate_pull_requests`.
- A removal never follows a link below its anchor: a Store whose registered path now leads
  through a symlink, or an entry replaced since inventory, is reported and left for the
  operator.
