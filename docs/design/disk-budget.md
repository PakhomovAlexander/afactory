# Disk budget: af keeps a bounded, visible amount of disk

Status: accepted for implementation (2026-10-07). One package, five parts (D1 to D5), one pull
request.

## 1. Problem

af keeps nearly every byte it writes, and nothing bounds the total. Measured on the maintainer's
Mac in the week to 2026-10-07:

- The warm Task-check cache made a new 4.7 GiB key for every check Attempt and never reused or
  removed one: 15 keys (65 GiB) in 26 hours, then 11 more (52 GiB) the next day. With a native
  toolchain mapping, `prepare_native_rust_toolchain` puts the check's fresh `tempfile::tempdir()`
  into `PATH` and `RUSTUP_HOME` (`crates/review-pipeline/src/task/code.rs:216-279`), and
  `toolchain_identity` hashes both (`crates/review-pipeline/src/task/warm_check.rs:323-357`). 13 of
  13 kernel checks ran `cold empty`. Without the mapping, one key served about 40 Attempts.
- `max_bytes` bounds one key. Nothing bounds the number of keys, the review campaign Stores (99,
  17 GiB), the Task Stores, or installed versions. ADR-0131 says "Old toolchain directories stay
  until an operator removes them"; `af task gc`, `af review gc` and `af self prune` exist but only
  run by hand.
- 14 of 19 Task Stores fail to open with "log was written by another af release"
  (`crates/review-core/src/event.rs:282`), so `af task gc` cannot collect them. Stores made with
  `--state` outside the state root are known to nothing.
- Checks have no writable place af owns. A Go check script copied the tree into
  `mktemp -d /tmp/...` on every gate run (1.1 GiB each, 52 GiB with its caches). Review gate checks
  run with `env_clear()` and no `HOME` (`crates/review-check/src/runner.rs:262`), so this repo's
  `scripts/verify.sh` builds a 16.7 GiB target under `/tmp/.cache`.
- Claude Workers get the user's real `CLAUDE_CONFIG_DIR` (`crates/af/src/providers/task.rs:99-140`)
  and run in a fresh directory, so every Attempt and every admission probe leaves a new
  `projects/<slug>` directory in the user's Claude history (351 of 404 on this Mac). Remote checks
  push `af-gate/<task>/{base,head}` and open a draft pull request that af never closes
  (`crates/review-pipeline/src/task/remote_check/github_pr.rs:6,76`).

A user who runs ten Workers for a week should not have to learn any of this. After this package, af
holds at most a configured budget (20 GiB by default), refuses work instead of filling the disk,
and leaves nothing in other tools' directories.

## 2. Fixed requirements

1. The warm cache key depends only on what decides the build: the toolchain's identity and the
   environment's values, never on a per-check or per-Attempt path. Two checks with the same
   toolchain, environment and repository share one key across Attempts, Tasks and af processes.
2. One machine-wide budget covers every byte af keeps between runs: warm build keys, warm
   Workspaces, review campaign Stores, Task Stores (in the state root and registered elsewhere), and
   installed versions. When af holds more than the budget it evicts least recently used entries
   first. It never evicts an entry in use: a held lock, a live writer lease, a running Task or
   campaign, the default or a pinned version, or the running binary.
3. Every `af task run` and `af review run`, whatever the outcome, ends with the sweep that holds af
   to the budget. Age-based collection (`keep_days`, `keep_tasks`, `keep_campaigns`) is off by
   default (`auto_gc = false`): nothing a user still has room for disappears by age alone; it runs
   on `af storage prune --apply`, or after every run when the operator turns `auto_gc` on. An
   interrupted run is not an outcome: after `Ctrl-C` af stops promptly and the next run or
   `af storage prune --apply` sweeps. The sweep reaches every Store af made, including Stores this
   release cannot read and Stores made with `--state`.
4. Every check af runs gets a `HOME`, a `TMPDIR` and an `AF_CHECK_SCRATCH` directory that af creates
   empty before the check and removes after it. A check never needs a path outside them, and a
   review gate check gets the same environment contract as a Task code check.
5. af removes what a Worker or a gate leaves in other tools: the Claude transcript of every Claude
   Attempt and admission probe, and the gate branches and draft pull request of every remote check.
   Removal never changes a Task's result; a failed removal is recorded and retried.
6. Below a free-disk floor af refuses to start a check or a Worker Attempt, before any provider
   token is spent, with a message that names the free bytes, the floor, `af storage` and the knob.
7. The repository cannot steer any of this: `[storage]` is read only from machine layers, as
   `[self]` is.
8. Nothing here weakens an existing safety rule: the suspect-directory inspection of warm keys,
   `af task gc`'s tombstones and lease checks, the gate's push rules, and every `--apply`
   preview-first command keep their behaviour.

## 3. Model

### 3.1 Configuration

A machine-only `[storage]` table in the configuration ladder (`crates/af/src/config.rs`). It is
stripped from the directory, project and local layers and reported in `ignored`, exactly like
`[self]` (`config.rs:328-336`). Built-in defaults:

```toml
[storage]
max_bytes = "20GiB"          # the budget; integer bytes or a string with B, KiB, MiB, GiB, TiB
min_free_bytes = "10GiB"     # the free-disk floor of 2.6
auto_gc = false              # also collect by age after every run (D3); off: budget only
keep_days = 14               # collection never takes anything used in the last keep_days
keep_tasks = 20              # newest finished Tasks kept per Store
keep_campaigns = 20          # newest review campaigns kept
keep_worker_transcripts = false
keep_gate_pull_requests = false
```

Environment overrides follow the existing pattern (`AF_STORAGE__MAX_BYTES`, `config.rs:348-377`).
A typed `StoragePolicy` with `Config::storage_policy()` mirrors `SelfPolicy`. `af config show
--origin` shows every value's origin.

### 3.2 Entries and their last use

The budget counts entries. An entry is the unit af evicts whole; its size is its allocated bytes
(`st_blocks * 512`, links not followed), so APFS clones and sparse files are not over-counted.

| Kind | Path | Last use | In use when |
| --- | --- | --- | --- |
| warm key | `$XDG_CACHE_HOME/af/task-build-cache/<project>/<toolchain>` | `warm.lock` mtime, touched on every acquire | its `warm.lock` cannot be try-locked |
| warm Workspace | `$XDG_CACHE_HOME/af/workspaces/<id>` | its newest write | its lock is held |
| review campaign | `$XDG_STATE_HOME/af/review/campaigns/c-*`, `review/local/*` | newest Store write | a live writer lease or a running campaign |
| finished Task | inside a Task Store | its last event | running, leased, or bound by another Task's inputs (the existing collection rules) |
| unreadable Store | a Task Store this release cannot replay | newest file mtime | any lock or lease file in it is held |
| installed version | `$XDG_DATA_HOME/af/versions/<v>` | install time | the default, a pin, or the running binary |

Task Stores are evicted Task by Task through the existing collection (`apply_task_collection`,
`crates/review-store/src/store/task/collection.rs:626`), so tombstones stay. A Store this release
cannot read is the one exception: it is removed whole, because it cannot be collected in part.

Sizes may come from a record af writes when it last finished writing an entry (a warm check
already measures its key; a Store can record its size when a run closes it), so a sweep does not
walk an unchanged cargo target. An entry with no current record is measured once.

### 3.3 The sweep

One function, used by every trigger:

1. Enumerate entries: the default roots plus the registry of 3.4.
2. Collection (when called with collection on): finished Tasks idle at least `keep_days` beyond the
   newest `keep_tasks` of each Store; campaigns idle at least `keep_days` beyond the newest
   `keep_campaigns`; unreadable Stores idle at least `keep_days`; remote gate leftovers of D5 not
   yet cleaned.
3. Budget: while the total exceeds `max_bytes`, evict the least recently used entry that is not in
   use and was not used in the last hour (a concurrent run may have just made it). Stop when the
   total fits or nothing is evictable, and say which.
4. Report every removal (kind, path, bytes, rule) on stderr and, for a Task run, as an observation
   of that Task.

Triggers: the end of every `af task run` and `af review run` (the budget, and collection when
`auto_gc` is on); a warm check that
would create a new key; the free-disk floor of 3.5; and `af storage prune --apply`.

### 3.4 Store registry

When af opens or creates a Task or review Store at a path outside the default roots (`--state`,
`--state-root`), it records the path, its kind, and the first and last use in
`$XDG_STATE_HOME/af/stores.toml`. Recording is best effort and never fails a command. The sweep
visits registered Stores and drops entries whose path no longer holds a Store.

### 3.5 Free-disk floor

Before a check starts and before a Worker Attempt starts, af reads the free bytes of the volume
that holds its temp directory and of the volume that holds `$XDG_CACHE_HOME` (`statvfs`; `nix`
`fs` is already enabled in `af`, `review-sandbox` and `review-store`). Below `min_free_bytes` it
runs the sweep once; still below, it refuses that check or Attempt with the typed reason
`insufficient_disk`. A refused check reports it as its result; a refused Worker Attempt ends before
the Worker starts and is charged nothing. A debug-only test hook may fake the free bytes, like
`AF_TEST_GC_STOP_AFTER_TOMBSTONES`.

### 3.6 `af storage`

`af storage` prints, per kind: entries, bytes, the oldest and newest last use, and the unreadable
or unregistered Stores with their reason; then the total against `max_bytes`, and the free bytes
against `min_free_bytes`. `af storage prune` previews the sweep with collection on; `--apply`
performs it; `--json` on both. The refusal of 3.5 and the docs name this command.

## 4. The package: D1 to D5

### D1. Stable warm-cache key

- `toolchain_identity` gets the check's runtime directory. Before hashing `PATH`, `RUSTUP_HOME` and
  every other environment value it hashes, it replaces each occurrence of the runtime path with a
  fixed token. When a native toolchain was prepared, its verified `content_digest`
  (`code.rs:246`) joins the hashed inputs. The domain moves to `af.task-build-cache.toolchain/2`,
  so every old key goes stale once and D2 evicts it.
- `WarmSession` (`warm_check.rs:499`) keeps one resolved key per distinct hashed environment
  instead of one `Option` per Attempt (`warm_check.rs:561-572`), so a second check with another
  environment (for example `markdownlint`) gets its own key.
- Tests: with a stub native toolchain mapping (`crates/af/tests/it/task_warm_checks.rs` helpers),
  two Attempts of the same check resolve the same key and the second reports `cargo_target warm`;
  two checks with different environments get different keys; a changed toolchain digest gets a new
  key.

### D2. One machine-wide budget, evicted least recently used

- `[storage]`, `StoragePolicy`, the entries of 3.2, the sweep of 3.3, the floor of 3.5, and
  `af storage` of 3.6.
- A warm check touches `warm.lock` on every acquire, records its key's measured size when it
  finishes, and runs the budget step of the sweep before it creates a new key.
- `af review gc` gains the liveness check it lacks (it removes campaign directories today without
  one): a campaign with a live writer lease or a running process is never removed, by `af review
  gc` or by the sweep.
- Tests: with a 1 MiB budget and small real files, the sweep evicts warm keys, campaigns, finished
  Tasks and versions oldest first and stops when the total fits; a locked key, a leased Store, the
  default version and an entry used within the hour are skipped; `[storage]` in `.af/af.toml` is
  ignored and reported; the faked free bytes below the floor refuse a check and a Worker Attempt
  with `insufficient_disk` and no spend; `af storage --json` and `af storage prune` (preview, then
  `--apply`) report what they did.

### D3. The sweep runs after every run, and collection reaches every Store

- At the end of `af task run` and `af review run`, whatever the outcome: the sweep's budget step,
  with collection on too when `auto_gc` is (off by default, decided on 2026-10-07 after a test run
  of this branch collected a developer's real review history: age alone is not a reason to delete
  what a user still has room for). Its failure is a warning, never the command's exit status.
- Stores this release cannot replay (the `ANOTHER_RELEASE` error, `event.rs:282`) are listed by
  `af storage` with that reason and removed whole by collection when idle at least `keep_days` and
  not in use.
- The registry of 3.4.
- Tests: with `auto_gc = true`, after a run an old finished Task beyond `keep_tasks` is collected
  and a recent one is kept; the default keeps both until `af storage prune --apply`; a Store fixture whose log holds an unknown event type is
  removed when idle and kept when recent or locked; a `--state` Store outside the root is registered
  and collected; a registry entry whose Store is gone is dropped.

### D4. Each check gets a scratch directory, a `HOME` and a `TMPDIR` that af owns

- Every check runs with `HOME=<runtime>/home`, `TMPDIR=<runtime>/tmp`,
  `AF_CHECK_SCRATCH=<runtime>/scratch` and `XDG_CACHE_HOME=<runtime>/cache`, all created empty and
  writable: Task code checks (cold, warm, and the local part of remote checks), measurement checks
  (`measure.rs:231`), review gate checks (`review_domain.rs:601`) and post-apply integration checks
  (`review_domain/integration.rs:676`, including container runs through `with_split_env`). Review
  gate checks also get the kernel's `RUSTUP_HOME` with `RUSTUP_AUTO_INSTALL=0`, as Task checks do
  (`code.rs:1116-1119`), so a private `HOME` never downloads a toolchain.
- Runtime directories are named `af-check-<pid>-<random>` under the temp directory, so the startup
  crash sweep (`crates/review-sandbox/src/stale.rs:97`) removes those of dead processes, as it does
  for `af-sandbox-*`. They are removed when the check ends, as today.
- Docs tell check authors to write to `$AF_CHECK_SCRATCH` or `$TMPDIR`, never to `/tmp`.
- Tests: a check script sees the four variables, each directory writable and inside the runtime,
  and none of them exists after the check; a review gate check sees a `HOME`; a stale `af-check-*`
  of a dead pid is removed at startup and one of a live pid is kept.

### D5. af removes what it leaves in other tools

- Claude: the working directory of a Claude Worker Attempt and of an admission probe is a fresh
  directory af created for that Attempt alone, so `<CLAUDE_CONFIG_DIR>/projects/<slug(cwd)>`
  belongs to that Attempt. When the Claude process exits, whatever the outcome, af removes that
  directory. The slug is the one `crates/review-runner-claude/src/session.rs:72-85` already
  computes. af never removes a directory whose slug does not come from a path af created for the
  Attempt. When the installed CLI accepts `--no-session-persistence` (Claude Code 2.1.292 does,
  with `--print`), af may pass it too; removal stays the contract because the CLI auto-updates.
  `keep_worker_transcripts = true` keeps transcripts.
- Remote checks: when a Task finishes (`task_execution.rs:2028`), and when collection takes a Task,
  af closes each draft gate pull request recorded in its `RemoteCheckEvidenceV1` and deletes
  `af-gate/<task>/{base,head}` from the mapping's push URL, with the same `gh` and `git push` tools
  the gate uses. The result is a `gate_cleanup` observation (done, or failed with the reason). A
  failure never changes the Task's result and is retried by the next sweep while the mapping still
  names the repository. The never-delete notes at `github_pr.rs:6,76` and the manual hint at
  `:256-258` change to say this. `keep_gate_pull_requests = true` keeps them.
- `af self uninstall --purge` also removes, in every `CLAUDE_CONFIG_DIR` of the provider registry,
  the project directories whose slug comes from an af-created path (`af-sandbox-*`, `af-check-*`,
  and the earlier `<temp>/.tmp*/tree` form), and every registered Store. It lists any gate branches
  and pull requests it could not reach.
- Tests (offline, with the fake CLIs the provider and remote-check tests already use): a fake
  Claude that writes `projects/<slug(cwd)>/<id>.jsonl` leaves nothing after an Attempt and after a
  probe, while another project directory is untouched; `keep_worker_transcripts = true` keeps it; a
  finished remote-check Task closes its pull request and deletes both branches; a failing `gh`
  records `gate_cleanup` failed, keeps the Task result, and a later sweep retries.

## 5. Documents

- A new ADR: "Hold af's disk use to a machine budget". It amends ADR-0131 (old keys are now
  evicted, not left to the operator) and ADR-0135 (collection now also runs by itself), and records
  the rejected alternatives below.
- `docs/tasks.md` (warm checks, reclaiming space), the configuration reference for `[storage]`,
  the `af storage` help and docs, the check-author guidance of D4, `CONTEXT.md` terms (Storage
  Budget, Store registry), and `changelog.d/disk-budget.md`. Never `CHANGELOG.md`.

## 6. Rejected alternatives

- **A larger per-key bound.** It does not bound the number of keys, which is what filled the disk.
- **Collection on every `af` command.** It would slow every command; collection runs at the end of
  runs, before a new warm key, at the free-disk floor, and on request.
- **A private `CLAUDE_CONFIG_DIR` per Worker.** The login lives with the config directory, so a
  throwaway one would not be signed in.
- **Leaving gate pull requests for the operator.** That is the behaviour that left #179, #180, #185
  and #187 open.
