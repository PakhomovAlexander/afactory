- A Worker's draft reply may spell its `citations` and `repository_citations` in any order: the
  runner admits both sets in canonical order (sorted, unique) before validating the reply, so an
  author is judged on what it cited rather than on the order it listed it. This repository's
  `kernel/report` gives its author two Attempts.
- Store hygiene and the warm cache's two bounds (ADR-0135, package R5 of
  `docs/design/research-pipelines.md`; amends ADR-0131). `af task list --sizes` prints each Task's
  CAS bytes — those only it reaches and those it shares with another Task or Campaign record —
  and the Store's total; `--json` adds `sizes` to each entry and `store` to the document. `af
  task gc --older-than DAYS --keep N` previews, writing nothing, which finished Tasks beyond the
  newest `N` it would collect and why every other Task stays (`running`, `unfinished`,
  `writer_lease`, `bound_by`, `kept_newest`, `kept_recent`); `--apply` takes the Store's writer
  lock, is refused while any writer lease is live, appends one `task_collected` transition
  carrying `af/TaskCollected@1` per Task — referencing no artifact — and then removes every CAS
  object no uncollected record reaches, by a conservative walk through every digest a record
  spells. A stopped sweep is finished by the next run. A collected Task's projection stops at
  its tombstone: `af task list` and `af task show` print `collected <time>` with the retained
  summary (`af/task-collected-inspection@1` for `show --json`, a `collected` member on
  `af/task-list-entry@2`), `af task output`, `deliver`, `run` and `explain` refuse it, and replay
  never calls its removed objects corrupt; the browser lists the Tasks it can open. An append now
  rechecks, under the writer lock, that every object it references is still filed. `[warm]
  max_bytes` is now the eviction bound, applied before a check and after one whose result
  stands, and a new `[warm] hard_max_bytes` (twice `max_bytes` by default, at most 32 GiB) is
  the only bound that ends a running check with `warm_cache_bound_exceeded`;
  `TaskCacheObservationV1` records the acting `bound` and `af task show` prints it. New schemas
  `task-collected-v1.json`, `task-collected-inspection-v1.json` and `task-gc-v1.json`;
  `task-transition-v5.json`, `task-list-entry-v2.json`, `task-runtime-evidence-v1.json` and
  `code-task-policy-v1.json` gain the optional members. A Store without a tombstone and a policy
  without `hard_max_bytes` behave as before, except that such a policy's check now fails only
  above twice its `max_bytes`. A reader that loses an artifact to a concurrent sweep reports the
  Task as collected, a listing projects the uncollected Tasks before reading the tombstones, an
  opening Task re-checks every Task its bindings name under the writer lock, and a collected
  Task's row under `--sizes` carries a zero footprint.
- Bind any declared root port (ADR-0134, package R4 of `docs/design/research-pipelines.md`;
  amends ADR-0117). A Task file's `inputs` table may bind any root input the selected Pipeline
  declares — the one the Task file names, or else every captured Pipeline accepting its kind,
  alike — to a recorded, finished Task's result output whose artifacts verify in the CAS and
  whose type and cardinality equal the port's exactly; `requirements`, `base` and `continuation`
  stay refused by name, and `source`, `history` and `sources` keep ADR-0117's rules. Only result
  outputs bind: naming an Attempt's raw artifacts, runtime evidence or any other record is
  refused with a message saying so, and an exact `{ "artifact" }` reference binds only
  ADR-0117's three ports. A list of one to sixteen `{ "task", "port" }` references binds a `many`
  port in order, so `measurements` can take an experiment's `baseline` and `candidate`; a `one`
  port keeps its output's Snapshot ID, a `many` port bound from several outputs names none while
  each artifact keeps its own, and a list into a `one` port is refused. Every refusal names the
  port and both types before any Worker or Provider admission. The Store, the executor (for a
  `many` port the consuming contract declares `unbound`) and the Worker renderer accept such a
  Snapshot-less `many` input port; output ports keep the one-Snapshot rule. A port bound without
  naming the Pipeline must be declared alike by every Pipeline accepting the kind, else the Task
  file is asked to name one; a list bound to a `one` port has each reference judged before its
  shape; a bound `many` output records every artifact it holds; and `source`, `history` and
  `sources` are checked against the selected Pipeline's declaration at resolution. This repository's `kernel/analyst` and `kernel/report-verifier` schemas take the
  `snapshot_id` a bound Measurement or comparison carries.
  `af/TaskInputBindings@1` records a port's further outputs in an optional `also` list, absent
  for every single-reference binding, and `af task explain` and `af task show` print their
  existing binding rows once per output. `task-file-v1.json` gains the list form and
  `task-input-bindings-v1.json` the `also` list. The `builtin/report` starter's Worker schemas
  admit a `snapshot_id` on `comparison` and `measurements` values. A Task file without `inputs`
  is unchanged.
- Report Tasks (ADR-0133, package R3 of `docs/design/research-pipelines.md`). The built-in kind
  `report` selects a new installed profile: an author reads the source Snapshot, the kernel
  renders its draft and resolves its repository citations against that exact Manifest, and an
  independent verifier on the same Snapshot accepts the report. Its captured policy is
  `af.report-task-policy/1`, named by `report_policy` in `.af/task-catalog.toml` (a newly
  declared `.af/report-policy.toml`). A Task file's `report_sources` captures
  `af/ReportSources@1` — the `af.document-sources/1` shape with zero to 256 entries, 256 KiB each
  and 512 KiB in total in a file of at most 640 KiB, the empty set when absent; the `sources`
  port is optional, and a Pipeline that binds nothing there seals against the empty set. An
  execute-checks Worker may add beside the source, never under a name the source holds. `af/DocumentDraft@2` adds
  `repository_citations` of `{ path, line? }`, spelled exactly as the Manifest spells them and
  rendered as `path` or `path:line`. The installed `report_seal`, `report_check` and
  `report_accept` operators record `af/ReportCheckReceipt@1` (with each failed citation's
  reason: `absent`, `directory`, `symlink`, `binary`, `line_out_of_range`),
  `af/ReportEvaluation@1` and `af/ReportVerification@1`, every one naming the source Snapshot;
  a verifier whose checks judged another Snapshot is refused at admission. An author whose
  effects are `read-source` and `execute-checks` gets ADR-0118's shell in a clone that seals
  nothing back. A report Task allows no `write-source`, has no `snapshot` output and is refused
  by `af task deliver` with a message naming `af task output --port report`; `af task show`
  prints the report's title, the verifier's outcome and the cited Snapshot. `af catalog init
  --profile report` emits the credential-free `builtin/report` starter, and this repository's
  `kernel/report` Pipeline with `kernel/analyst` and `kernel/report-verifier` is staged in
  `fixtures/kernel-report/` for installation into `.af/`. New schemas: `report-sources-v1.json`,
  `document-draft-v2.json`, `report-check-receipt-v1.json`, `report-evaluation-v1.json`,
  `report-verification-v1.json` and `report-task-policy-v1.json`; the catalog, Task-file,
  Task-kind and operator schemas gain the new fields, profile and operators. Document and
  implement Tasks are unchanged.
- Measure and compare (ADR-0132, package R2 of `docs/design/research-pipelines.md`). A code
  policy may declare `[measures.<name>]` — a command, 1 to 16 `repetitions`, `warm`, `wall_ms`
  per repetition and `metrics` of `{ key, unit }` in `ms`, `bytes`, `count` or `ratio` — and
  `[objectives.<name>]` — a measure, a metric, `lower` or `higher`, `min_improvement_ratio`
  (decimal text such as `"0.1"` or the integer 0 or 1; a float is refused because the parser has
  rounded it) and `min_repetitions`. The installed `measure` operator runs a measure against a
  fresh read-only Snapshot per repetition with a private `HOME`, `TMPDIR`, `XDG_CACHE_HOME` and,
  unless `warm = true` binds the Warm Check Cache, `CARGO_TARGET_DIR`. It re-verifies the source
  after every repetition and records `af/Measurement@1`: every run's elapsed time, exit status,
  output digests, the cache condition it actually had (warm or cold, bytes, and why when cold)
  and the metrics the command reported on an `af.measure-report/1` last line with the declared
  keys and units. A repetition the kernel ends at a time bound is recorded from the supervisor's
  typed ending as `timeout` or `deadline`, with what it printed kept, and a command a signal
  ended records no exit code. A failure (`exit`, `timeout`, `deadline`,
  `malformed_report`, `unit_mismatch`, `source_mutated`) stops the measurement and leaves no
  summary. The installed `compare` operator folds two Measurements into
  `af/MeasurementComparison@1` in exact decimal arithmetic: medians with an exact even-sample
  mean, signed improvements, ratios in lowest terms, and `improved`, `below_threshold`,
  `unchanged`, `regressed` or `inconclusive` per metric. It is `passed` only for `improved` on
  the objective's metric. The plan compiler refuses a measure node whose repetitions exceed
  `check_wall_ms`, undeclared measures and objectives, and mixed comparisons. `af task output
  --port comparison --format markdown` renders one table, `af task show` prints medians and
  conclusions, and `af catalog init --profile experiment` emits the `builtin/experiment`
  starter, whose evaluator runs only after passed checks and a passed comparison.
  `scripts/measure-release.sh` and this repository's `release_build` measure,
  `release_build_time` objective and `kernel/experiment` packages are staged in
  `fixtures/kernel-experiment/` for installation into `.af/`. New schemas:
  `measurement-v1.json` and `measurement-comparison-v1.json`; `code-task-policy-v1.json` and the
  operator schema gain the new tables and operators. A policy without them is captured, planned
  and shown exactly as before.
- Warm Task checks bind the kernel's rustup home and keep Cargo's home warm (ADR-0131 amended,
  package R1 of `docs/design/research-pipelines.md`). Under `[warm]` a check and its toolchain
  probe receive `RUSTUP_HOME`, from the kernel's own, else its `HOME`'s `.rustup`, and
  `RUSTUP_AUTO_INSTALL=0`. A rustup proxy therefore answers from the installed toolchain
  instead of downloading one into the check's fresh `HOME`. That download is what made the
  probe exceed its 30 s bound. `RUSTUP_HOME` joins the toolchain key, and where it came from,
  or why it is unset, is recorded. `build_cache` admits `cargo_home`, bound as `CARGO_HOME`
  beside `cargo_target` under one toolchain key and one shared `max_bytes`. A `cargo_home`
  holding `credentials.toml` is suspect. A declared `caches = ["cargo"]` supersedes it
  (`cargo_home:superseded`). Four verification findings are closed. First, the directories are
  measured again after every check, so a fast check that wrote past the bound fails with
  `warm_cache_bound_exceeded` too. Second, traversal is descriptor-relative
  (`openat`/`fstatat`, `O_NOFOLLOW`) and fails closed: an unreadable subtree makes a
  directory suspect before reuse and fails a check that left it. Third, `ensure()` reports a
  discard, and bytes are measured only after it, so a recreated directory is cold, never its
  old size. Fourth, every warm check, started or not, keeps one evidence group naming it
  (`TaskRuntimeEvidence@1` gains an optional `check` binding) with one observation per
  declared kind, such as `deadline_exhausted` or `cache_refused`. `af task show` never prints
  an unnamed line. A checked-in golden recorded by a kernel without this package pins every
  document of a Task without `[warm]`, and a fixture proves from the implementer's sandbox
  manifest, the sealed candidate, the derived Snapshot and the delivered tree that no cache
  byte reaches them.
- Warm Task checks (ADR-0131, `docs/design/research-pipelines.md` package R1). A code policy
  may declare `[warm] build_cache = ["cargo_target"]`, `caches = ["cargo"]` and `max_bytes`
  (default 8 GiB, at most 32 GiB). A `trusted_local` Task check then builds into
  `$XDG_CACHE_HOME/af/task-build-cache/<project>/<toolchain>/cargo_target`. The toolchain key
  digests the Snapshot's `rust-toolchain.toml`, `rustc -vV`, `cargo -vV`, the host triple and
  the check's fixed environment. The directory has one exclusive lock; a check that waits 60 s
  for it runs cold. It is bounded before, during (`warm_cache_bound_exceeded`) and after every
  check, and removed rather than repaired. Declared Cache Snapshots bind `CARGO_HOME` from the
  check's runtime directory. `[warm]` with `require_container = true` is refused at load. Each
  warm check records its own `TaskRuntimeEvidence@1` with one cache observation per kind, whose
  `kind` may carry a `:reason` suffix. `af task show` prints `check <name>: <ms> ms, cargo_target
  warm <bytes>` or `cold <reason>`. `schemas/code-task-policy-v1.json` is new. `scripts/verify.sh`
  honours a `CARGO_TARGET_DIR` that is already set. A policy without `[warm]` is captured, run
  and shown exactly as before. A check holds an exclusive lock over its whole toolchain key from preparation to the end of
  its removal step, the bound is measured over the whole key, a directory that is suspect once
  the check ended (a link, a special file, a forbidden `credentials.toml`, a root swapped for a
  link) fails the check with `warm_cache_suspect` and is removed under the lock, and a root that
  is no longer a real directory counts as above every bound. Every compile-time
  `CARGO_MANIFEST_DIR` in the workspace is now a fallback behind the run-time `AF_WORKSPACE_ROOT`,
  because a warm gate reuses test binaries compiled in the previous gate's sandbox, and a test
  refuses a new one; this repository bounds its warm cache at 16 GiB. Every operation below a
  toolchain key goes through the key directory's descriptor, never a path, so a check that swaps
  the key's parent for a link cannot redirect cleanup; the shared bound is measured over the whole
  key before and after every check and an eviction removes every kind below it; and an
  observation's `evicted_bytes` carries its `evicted_reason`, which `af task show` prints. Only the
  two warm kinds are ever locked and only the kernel's own lock files are exempt from the bound
  (they are truncated on acquisition), a policy whose kinds a Cache Snapshot supersedes still holds
  and bounds the key, and a check whose warm directory is suspect once it ended fails even when
  nothing was left to evict. Only the lock inodes the holder opened are exempt from the key's
  bound, the key directory itself must stay private and its held locks in place for a check to be
  accepted (a widened key is emptied before reuse, never repaired), and a check that holds the
  key is monitored and judged even when every declared kind is superseded. A held lock is exempt
  only at its own name with its own inode and must stay a single empty name; the key is made
  writable through its held descriptor before an eviction, entries are removed by their exact
  bytes, and an eviction that leaves anything behind is an error; every declared observation of a
  failed check carries the eviction and its cause. A lock's inode is judged a plain, singly linked
  file of this user at its name before acquisition writes through it, a key that a check renamed
  and recreated is displaced and suspect, eviction empties or drops the held locks and reports
  success only when nothing but sound empty locks remains, and suspicion outranks the byte count
  in the recorded cause. ADR-0131 states where these rules stop: `trusted_local` is not
  isolation, and the cache is honest as evidence, not a defence against a check acting on the host. A link or a
  directory at a lock's name is removed before the name is opened, and a waiter judges the inode
  again after acquiring the lock, before truncating it. A held lock a check grows counts toward
  the running bound, removal addresses every entry by its exact bytes, and a directory's
  `source_digest` no longer varies with the lookup's outcome.
