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
5. **One machine-wide budget over entries af evicts whole, least recently used first, plus a
   free-disk floor, collection after runs, af-owned check directories and removal of what
   Workers and gates leave in other tools.** Chosen.

## Decision

1. **A stable warm key.** `toolchain_identity` replaces the check's runtime path, as given and as
   resolved, with a fixed token in every environment value it hashes, and hashes a prepared
   native toolchain's verified content digest; the domain is `af.task-build-cache.toolchain/2`,
   so every older key goes stale once and the budget evicts it. A check Attempt resolves one key
   per distinct hashed environment, so two checks with different environments get different
   keys. Two checks with the same toolchain, environment and repository share one key across
   Attempts, Tasks and processes.
2. **`[storage]`, machine layers only.** A typed `StoragePolicy` with built-in defaults:
   `max_bytes = "20GiB"`, `min_free_bytes = "10GiB"`, `auto_gc = true`, `keep_days = 14`,
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
   whole), and installed versions. Sizes are allocated bytes, links not followed; the files a
   reader creates (`-shm`, an empty `-wal`) never count as use.
4. **One sweep.** Collection first, when asked: gate leftovers of finished Tasks, finished Tasks
   beyond the newest `keep_tasks` of each Store, campaigns beyond the newest `keep_campaigns`, and
   unreadable Stores, each idle at least `keep_days`. Then the budget: while af holds more than
   `max_bytes`, the least recently used entry goes. Never an entry in use (a held lock, a live
   writer lease, a running `af review run`, the default or a pinned version, the running binary)
   and never one used within the last hour. It stops when the total fits or nothing else may go,
   and says which. Every removal is reported on stderr. Triggers: the end of every `af task run`,
   `af task start --execute` and `af review run` whatever the outcome (when `auto_gc`; its failure
   is a warning), a warm check about to create a key that does not exist (budget only), the
   free-disk floor, and `af storage prune --apply`. An unchanged Task Store whose last collection
   found nothing to do before a known time is not planned again until then.
5. **The free-disk floor.** Before a check and before a Worker or Provider-probe Attempt starts,
   af reads the free bytes of the volumes holding its temporary directory and `$XDG_CACHE_HOME`.
   Below `min_free_bytes` it sweeps once; still below, a check is `not_run` with reason
   `insufficient_disk: …`, and a Worker Attempt is released before it starts, charged nothing,
   and the run stops like an interrupted one: nothing is assembled or finished, and the Task stays
   resumable. The message names the free bytes, the floor, `af storage` and the knob.
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
9. **What Workers and gates leave in other tools.** When a Claude Worker Attempt's or admission
   probe's process exits, whatever the outcome, af removes `<CLAUDE_CONFIG_DIR>/projects/<slug>`
   of its working directory — only when that directory is an `af-sandbox-*` or `af-check-*`
   directory af made for that Attempt, never another; `keep_worker_transcripts` keeps them. When
   a Task finishes, and before collection takes a Task, af closes each draft gate pull request its
   evidence recorded (still between this Task's two branches) and deletes whichever of
   `af-gate/<task-id>/{base,head}` the mapping's push target still has, with the gate's own `gh`
   and `git`, never force and never another ref. The result is one `gate_cleanup` transition
   carrying `af/TaskGateCleanup@1` inline: done, or failed with its redacted reason. It never
   changes the Task's result; a failed one is retried by the next sweep while the mapping still
   names the repository; `keep_gate_pull_requests` keeps everything. `af self uninstall --purge`
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
- `TaskTransition@5` gains the `gate_cleanup` change and `af/task-inspection@11` the optional
  `gate_cleanups` list; a release before this one cannot read a log that holds one.
- Removals of a sweep are reported on stderr and in `af storage prune --json`; they are not yet
  recorded as observations of the Task whose run triggered them.
