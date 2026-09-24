# Changelog

Every release has a section here before it is tagged: `scripts/release.sh X.Y.Z --compat "…"`
writes it from the pull requests merged since the previous release, and the release workflow
publishes the section as the release notes. The **Authority compatibility** line is mandatory:
it says whether committed `.af/` policy keeps working as is, needs `af onboard --refresh-lock`,
or needs a documented hand edit.

This file starts at the first GA release. The pre-GA `0.x` releases are described on their
[GitHub release pages](https://github.com/PakhomovAlexander/afactory/releases), and git history
keeps their sections. In short: `0.7.0` and `0.7.1` reviewed from a `.review/` directory;
`0.8.0` moved project authority to `.af/` and made a project pin the `af` release it runs; the
`0.9.0` release candidates brought the common Task runtime, Task files and the Worker warm
layers. `0.9.0-rc.5` was tagged but never published — its macOS check leg failed before the
publish step — so everything it carried shipped in `0.9.0-rc.6`, the last pre-GA release.

## [Unreleased]

- Bare `af` at a terminal opens a read-first, vim-shaped browser (ADR-0119,
  `docs/design/tui.md` package M1). On a pipe, bare `af` still prints help to stderr and exits
  2. The scope comes from `config::load`: the user scope outside a repository, the project scope
  inside one, or `af --repo DIR`. A 28-column folding bar holds `providers/`, `workers/`,
  `pipelines/` and `tasks/`. Beside it, the Settings pane shows the layer table and the effective
  values with their origins, as `af config paths` and `af config show --origin` print them, and
  `e` opens a layer with `af config edit`. The Providers pane shows the `af provider status`
  columns with a bar per quota window, and `R` runs the bounded usage probe off the key loop. The
  Pipelines pane shows, verbatim, the `af task explain --tree` text of a token-free plan compiled
  as `af task plan` compiles one, into a scratch Store; the status line shows the highlighted
  slot's Worker binding. The Workers and Tasks panes arrive in later packages. `:` lines are
  parsed by the CLI's own clap definition. The terminal runs through `rustix::termios`, an
  existing dependency, instead of the planned `crossterm`. No `--json` document, schema or
  fixture changes. Discovery under `.af/task-packages/` walks every real directory, however deep, and keeps walking below a package. Escape sequences split across terminal reads decode whole (`tui::keymap::Decoder`), and a lone `ESC` is Escape only after a read brought nothing. Outside a repository the user scope never opens a directory layer, so a broken `.af/af.toml` above a plain directory cannot block it. `af --repo DIR` with no subcommand is exempt from pin dispatch like bare `af`. The bar lists the pipelines `HEAD` commits and reads every declaration from `HEAD`, the authority a preview compiles, marking a working-tree file that differs; a package the kernel refuses to plan shows its declared contract; a review Pipeline shows its committed declaration. `af config edit user` names the user layer without reading the ladder, so a directory layer that does not parse cannot block it. `e` on a highlighted layer opens that exact file, so two present directory layers cannot swap; a settings refresh or an editor hand-off whose reload fails says the settings are stale and why, instead of calling them refreshed; the Worker-binding status follows a modified package's notice row; a malformed escape stream is bounded and dropped through its terminator. Git runs for the pipelines pane with a cleared environment and an allowlist, so an inherited `GIT_DIR` cannot redirect it; a failed read of `HEAD` is an error above the last good entries, not an empty catalog; entries are bound to one resolved commit and read again before a preview when `HEAD` moved; a package is previewed only when the committed catalog pins its name at that file; an editor hand-off refreshes the opened pane; the status line yields the breadcrumb before cutting a binding or an error at 80 columns. A preview compiles the exact commit its entries were read from (`plan_tree_preview_at`), and a result for a commit the pane left is dropped; drift is judged by `git diff` (bytes, mode, existence); a failed `HEAD` read is shown on an opened entry too and nothing is compiled from stale entries; a scope change drops the previous repository's entries; the terminal's panic hook restores the screen only from the thread that owns it.
- Let a review Worker that declares `execute-checks` build and drive the candidate (ADR-0118). A
  model Worker whose `roles` contain `review` and whose effects are
  `["read-source", "execute-checks"]` now runs in the same `Mode::EphemeralWrite` clone that
  AF-owned preparation uses, with its readable Review inputs, instead of a read-only tree. The
  Claude adapter adds `Bash` to its adapter-owned `--tools` and `--allowedTools` and keeps
  `--safe-mode --restricted --permission-mode dontAsk --strict-mcp-config`. Codex runs
  `-s workspace-write` rooted at the sandbox. The model process's whole group is killed when it
  exits, so a shell child cannot outlive the Attempt; wall-clock and token bounds are unchanged.
  Nothing is sealed back. At `finish` every Snapshot entry must be byte-identical; anything the
  reviewer added — build output, its harness, the dotfiles tools write into `HOME`, which is the
  sandbox root — is discarded with the clone and is not a source edit.
  Any source edit fails the Attempt with a diagnostic naming the changed paths, and a read-only
  Worker's refusal names its paths the same way. The adapter's `writable: bool` became the
  kernel-derived `review_runner::task::WorkerAccess`, computed from the captured effects by
  `review_pipeline::task::source::worker_access`, so no package or `.af/` policy can name a tool.
  Existing Workers derive exactly the access they had; fixtures and `--json` documents are
  unchanged. A command Worker declaring the same effect runs under the process-group-killing
  exit policy too, so a background child it starts cannot outlive its Attempt
  (`crates/review-runner/tests/task_command_process_group.rs`).
- Kill a supervised child's process group before reaping the child, not after: a reaped pid is
  free for reuse, so the late `SIGKILL` could land on an unrelated process that had just been
  spawned into its own group under the recycled id — on a loaded machine, a fresh `git rev-parse`
  dying with an empty stderr. `review-process` now observes the exit without reaping
  (`waitid(WNOWAIT)` on Linux, kqueue `NOTE_EXIT` on macOS and the BSDs), routes every group
  kill through the unreaped leader, and reaps last on every path — deadline, cancellation and
  held-pipe cleanup included; the provider probes poll the same way, ending a probe's group
  before reaping it. A failed git command with no stderr now reports its exit status, so a
  signalled git reads as signalled. The two provider lock files are opened with a
  bounded retry on the spurious `ENOENT` APFS returns to a concurrent `openat(O_CREAT)`.
- Declare the `.af/` layout once and keep Task files out of git (ADR-0115): every canonical entry
  under `.af/` — its kind, its writer of record, whether git versions it and what it holds — is
  now one table in `review_config::layout`, and `layout::classify` answers whether any
  repository-relative path is declared or undeclared. `af help config` renders that table and one
  paragraph saying what never belongs under `.af/` and where it lives instead, and the old claim that `af init`
  gitignores `af.local.toml` is gone — there is no `af init`, and no command writes a `.gitignore`. A test walks every production source under `crates/*/src` and
  fails on any `.af/` path the table does not declare, which added `document-policy.toml`,
  `packages/`, `artifact-reuse/`, `cache/` and the in-memory-only `task-compat/` to the declared
  set. `af task plan`, `af task start --file`, `af review plan --file` and `af review run --file`
  now warn on stderr, after capture, when the Task file resolves inside the repository and `git
  check-ignore` does not ignore it, naming the file and `$XDG_STATE_HOME/af/tasks/`. The warning
  is advisory: exit codes, stdout and every `--json` document are unchanged, and a Task file that
  is gitignored or outside the repository draws no warning.
- Report undeclared `.af/` paths and let a project refuse to deliver them (ADR-0116):
  `review_config::layout::classify_manifest` groups every `.af/` path of a captured Snapshot
  manifest into declared and undeclared, with a path count and a byte total for each. It is a
  pure function of the manifest and the L1 table — it reads recorded entries, never a working
  tree or a sandbox, and judges a path by its decoded bytes while keeping the manifest spelling —
  so the same Snapshot answers the same on every machine. `af task plan` and `af task start` now
  print one advisory line naming the count, the byte total and up to ten paths, and carry the
  whole group in a typed `undeclared_af_paths` field of the `--json` document; `af task deliver`
  records it in `af/task-delivery@1` beside `ignored_paths` and prints it in the delivery
  summary. A new project policy `[delivery] undeclared_af_paths = "warn" | "refuse"` in
  `.af/af.toml` defaults to `warn`; under `refuse`, delivery fails before the prepared record and
  before any Git mutation, naming every undeclared path and leaving no branch, no worktree and no
  record. The policy is captured into the Task's project policy identity at plan time and read
  back from that record at delivery, so editing `.af/af.toml` afterwards cannot change an
  admitted plan, while the default is not written down and moves no existing policy identity.
  Nothing is removed: Snapshot identity, the delivered tree and `ignored_paths` are exactly what
  they were, receipts written before the field deserialize with an empty group, and a repository
  whose authority tree is fully declared reports nothing at all.
- Decide how a Task file binds a root input port to a recorded Task's output (ADR-0117, proposed;
  documentation only, no behaviour changes yet). A Task file gains one optional `inputs` table
  mapping `source`, `history` or `sources` to `{ "task": "<task_id>", "port": "<output port>" }`
  or to an exact `{ "artifact": "sha256:…" }`, so chaining implementation to repair to review no
  longer means exporting a candidate tree and reviewer results to files that then ride along in
  every later Snapshot. References resolve once, at plan time, from the `--state` Store into exact
  artifact IDs in the compiled plan, so resume, retry and replay never read the referencing Task
  file again; type and cardinality are checked against the port before any Worker or Provider
  admission, and the compiler's existing root-input check remains the one that cannot be
  bypassed. The referenced Task must be recorded and finished with the named port in its result,
  but need not be verified — the reference carries provenance only, and no acceptance,
  verification, plan approval, delivery or budget authority crosses. A bound `source` that is
  already a root capture is carried verbatim and delivers as usual; a derived one — what a
  `snapshot` output is — is republished as a root Snapshot over the identical Manifest with a new
  `af.task-source-origin/2` origin that names the referenced Task, result and port and carries no
  `source_revision`, so `af task deliver` refuses it for the one honest reason: a derived tree has
  no commit for the target's `HEAD` to equal. ADR-0031's exact comparison is unchanged. The Task-file shape,
  the display in `af task explain` and `af task show`, and the implementing package's crates,
  types and tests are written up in `docs/task-execution/task-inputs.md`, linked from
  `docs/README.md`.
- Bind a Task input port to a recorded Task's output (ADR-0117, accepted; the behaviour the
  entry above decided). A Task file's optional `inputs` table now resolves at plan time, from
  the `--state` Store only, into ordinary root ports: the referenced Task must be recorded and
  `Finished`, its result must read and validate, the named port must be in `result.outputs` and
  every artifact it names must verify in the CAS, and the recorded type and cardinality must
  equal the destination port's exactly — a `many` output never binds a `one` port however many
  artifacts it holds. `requirements`, `base`, `continuation` and any other name are refused by
  name. Every refusal is an ordinary Task-file input error naming the Task and the port, exit 1
  with `af/error@1` under `--json`, raised while the revision is still being built and therefore
  before any Worker dispatch or Provider admission. A bound `source` that is already a root
  capture is carried verbatim; a derived one is republished as a root Snapshot over the
  identical Manifest with a new `af.task-source-origin/2` origin and a new `af/SourceTree@1`
  envelope, and a parentless generation-2 Snapshot referenced again is carried verbatim, so
  nothing is re-rooted twice. `history` and `sources` carry the referenced artifact ID verbatim;
  a bound `history` suppresses the `empty_review_history` root default and a bound `sources`
  makes `document_sources` unnecessary. One `af/TaskInputBindings@1` artifact records the
  binding from `TaskRevisionV1.provenance.input_artifact_ids`, written only when the Task file
  carried an `inputs` table; `af/TaskRevision@1` is unchanged and a Task without bindings keeps
  its exact revision, plan and `--json` documents. `af task explain` annotates the `IN` line and
  `--tree` adds `BOUND` rows, `af task show` prints one `bound <port> <- …` line and advances
  its `--json` document gains an `input_bindings` field only when a binding exists, which the
  self-optimizer's AF history adapter admits without changing how accounting is read. `af task
  deliver` reads either origin generation and refuses a re-rooted source before the prepared
  record and any Git mutation, naming the referenced Task and port or the artifact ID: a derived
  tree has no commit for the target's `HEAD` to equal. ADR-0031's exact comparison is unchanged,
  and no acceptance, verification, plan approval, delivery or budget authority crosses a Task
  boundary. One limit is recorded rather than worked around: a bound `history` feeds a Pipeline
  that *reads* the ledger, not one whose Review would continue the predecessor's Round, because
  restoring a Round recomputes it and that recomputation requires every reviewer result to
  retain the current Task's `af/Requirements@1` — a rule this change does not relax.
  `docs/task-execution/task-inputs.md` says so and the fixture is built that way. Seven review
  findings were then repaired in place, each with the regression its reviewer asked for: a bound
  `source` is admitted only when the referenced envelope's payload, its `subject_snapshot_id` and
  the recorded port name one Snapshot whose origin belongs to the tree it describes; a refreshed
  revision keeps the `af/TaskInputBindings@1` record while replacing only its requirements
  artifact, and the Store's refresh validator derives the same expected list; selection preserves
  that record alone, so a Task with other non-port provenance and no `inputs` table keeps
  byte-identical revision, plan and inspection documents; the Claude usage probe observes its
  leader's exit without reaping and ends the process group before waiting, so a same-group
  descendant cannot outlive it; every untrusted label a refusal echoes — destination port,
  Task ID, output port, artifact spelling and the bindable-port reason — goes through the
  preview's display sanitizer; the `af/task-inspection@11` schema gains the optional `input_bindings` property. A command Worker declaring the same effect runs under the process-group-killing exit policy too, so a background child it starts cannot outlive its Attempt (`crates/review-runner/tests/task_command_process_group.rs`).

## [0.9.0-rc.6] - 2026-09-21

### Upgrading from 0.x

- GA reads only what GA writes
  ([ADR-0113](docs/adr/0113-ga-reads-only-what-ga-writes.md)). Review Campaigns and Tasks that a
  0.x release wrote are not supported (they may be refused or misread): that covers everything
  under `$XDG_STATE_HOME/af/review/` and `$XDG_STATE_HOME/af/task/`, and any directory passed with
  `--state` or `--state-root`. Before upgrading, finish or abandon in-flight Campaigns and Tasks
  with the release that started them, then delete that state. Committed `.af/` files are read only
  in the shapes this release accepts. A key or shorthand that only an earlier release wrote is
  refused: delete the refused key by hand, because `af onboard --refresh-lock` cannot repair a
  file it cannot parse.
- Removed `af review tui`. It read Worker pins only from the lock's legacy `[reviewers]` table,
  so it failed on every lock this release writes. The subcommand is now a usage error, and the
  release no longer ships its `af-review-tui.1` man page.
- Removed `af onboard --migrate` and the `.review/` to `.af/` conversion; the retired `.review/`
  layout is not read at all. The flag is now a usage error. `af onboard` on a repository that
  still carries `.review/` scaffolds `.af/` as for any other repository, a `.review/…` pipeline
  path gets the generic "must live under `.af/pipelines/`" error, and Campaigns whose manifests
  pinned `.review/` paths (af 0.7 and earlier) can no longer be resumed, reported or compiled into
  a Task.
- `.af/af.lock` no longer has a `[reviewers]` table or the 0.7.1 top-level `af_version` key, and
  `.af/af.toml` no longer has `[worker.*]` tables: this release refuses a file that still carries
  them, so delete those lines by hand; `af onboard` no longer writes `[reviewers]` or
  `[worker.*]`. Worker pins live under `[workers]` and the release pin under `[af]`, as before.
  A Worker package's `reviewer.toml` must now declare `subjects`; an omitted list no longer means
  whole-tree only.
- `af review run|plan|render`, the `af review` shorthand and `af provider doctor` no longer accept
  `--authority REV` or `--light`; both are usage errors now. Write `--policy-rev REV`, adding
  `--base REV` for a diff pipeline (a whole-tree pipeline still refuses `--base`), and drop
  `--light`, which only restated the default. The `af/review-plan@1` document no longer carries
  `selectors.compatibility_authority`, and the text plan drops its `compat` line.
- `af provider setup` no longer starts an official Provider CLI login on its own — add
  `--login`, and run it at an interactive terminal — and `af provider status` no longer probes
  subscription and quota windows unless asked with `--usage`. Both print the command or flag to
  use, so an existing habit fails loudly rather than silently. This is the machine-local
  `af provider` surface, not repository authority: the machine-local registry stays version 1,
  and its transaction, lock, publication and recovery protocol is unchanged.
- Default Campaign state resolves only the opaque `c-<id>` directory under
  `$XDG_STATE_HOME/af/review/campaigns/`; a directory there named by the label (the layout af 0.4
  and earlier wrote) is no longer a fallback. `af review campaigns|gc --state-root` still list an
  explicit `--state` directory named by its label. A Campaign you placed under that root yourself
  with `--state …/campaigns/<label>` must keep being addressed with `--state`: omitting it starts a
  new `c-<id>` Campaign, and `af review campaigns|gc` then refuse the root because one label holds
  state under both names.
- Removed `af help trust` and its `af-trust.7` man page, which described an `af trust` command that
  never shipped.
- `af self` and `install.sh` no longer install, activate or dispatch to releases older than 0.8.0,
  the first release with a signed `SHA256SUMS`: `af self install 0.7.x` (or a 0.8.0 release
  candidate) is refused, and a project whose lock pins one runs the current `af` instead, with a
  warning. A binary that embeds the release key, and `install.sh` with `minisign` on PATH, now
  refuse any release whose `SHA256SUMS` is unsigned, instead of accepting a pre-0.8.0 release on its
  checksums alone.
- The pre-rename `~/.config/afactory/` directory is no longer read, and nothing warns about it:
  move `providers.toml` and `caches.toml` from there to `~/.config/af/` (or
  `$XDG_CONFIG_HOME/af/`), or `af` finds no provider registry and no cache policy. Setting
  `AFACTORY_CACHE_POLICY_FILE` is no longer an error; it is ignored, so use
  `AF_CACHE_POLICY_FILE`.
- `af task list`, `af task show` and `af task deliver` read only the common `events.sqlite` Task
  store. Implementation Tasks that af 0.8.x and earlier kept in `tasks.sqlite` no longer appear,
  `af task show` no longer emits `af/task-inspection@1`, and delivery no longer knows the
  `refs/afactory/deliveries/<task>` ownership ref. In `af/task-inspection` and `af/task-list`
  output, every delivery preparation and receipt now carries `result_id` and every receipt carries
  `ignored_paths`, as this release always wrote them; the published schema requires both, and a
  Task whose stored receipt lacks one is refused.
- `af task start` now requires `--file`: `--kind implement --goal …` and `--pipeline` are usage
  errors. The fixed implementation v1 format they read, `.af/pipelines/implement.toml` with its
  `.af/workers/` implementer and evaluator packages, is no longer read, and `.af/af.toml` no longer
  accepts `defaults.task_pipeline`: every `af` command refuses a project file that still sets it,
  so delete that line by hand. The `implement` pipeline, its two Worker packages and their pins in
  `.af/af.lock` are then unused and can go too. Run implementation Tasks from a Task file against
  a Task catalog instead; `af catalog init --profile software --destination <new-dir>` creates a
  new starter directory with a runnable catalog and Task files. The `make pilot-check` target is
  gone; `make check` runs the same delivery and recovery tests.
- A Task catalog Worker's `worker.toml` can no longer declare
  `runner.kind = "legacy_task_command"` with its `protocol` and `legacy_budget_tokens` keys: the
  catalog refuses such a package. That runner spoke the fixed implementation v1 Markdown and
  verdict protocol. Declare a `command` or `model` runner instead, which reads
  `af.worker-request/1` and replies with `af.worker-reply/1`. Worker context is always
  `af/TaskContext@1`; `af/TaskContext@2` is neither written nor read.
- Task Review has one generation. A Task catalog whose `[review]` table omits `generation` now
  captures `af.review-task-policy/2`, the same policy as `generation = 2`, instead of generation
  one; any other value is still refused. Reviewer packages must use the generation-two ports: an
  `assignment` input of type `af/TaskReviewAssignment@1`, a `subject` of type
  `af/TaskReviewSubject@2`, and a `review.kernel/ReviewerResult@2` result that lists
  `dispositions` (one per assigned prior Finding) instead of `disputes`. The Review pipeline wires
  each reviewer's `assignment` from `review-bind`. A package with the old
  `af/TaskReviewSubject@1` or `review.kernel/ReviewerResult@1` ports, or without an assignment, is
  refused at planning, so update it together with its pin. The `af/TaskReviewSubject@1` contract
  and its `task-review-subject-v1.json` schema are gone.
- Task execution records have one encoding per record kind: the combined `prepared` record is
  gone, and an Attempt's context is bound through separate `reserved` and `context_bound`
  records. Settlements and usage observations carry decimal-string charges.
  `af/TaskTransition` drops `revision_recorded`, which no release wrote, and
  requires `revocation_id` on `approval_revoked`, which this release always writes.
- `af self optimize` history sources: the `af` adapter reads only the `af/task-inspection`
  receipts that `af task show --json` prints and refuses any other line, including the
  `af.task-event/1` event export that no af command produced. The `af`, `codex` and `claude`
  adapters no longer read normalized records (receipted as `legacy-normalized-v1`): `af` refuses
  such a line, and `codex` and `claude` take no observation from it. Label such a source
  `adapter = "external"` and give it a new `source_id`, because a retained source cannot change
  adapter. Report-only and `--experiment` requests now derive their `optimize-…` Task ID
  the same way light requests do, so re-running one whose capture an earlier release took starts a
  new Task.
- `af/TaskRuntimeEvidence@1` cache observations no longer carry `layer` and `result`, which were
  always `dependency_preparation` and `prepared`, and a runtime span's `kind` is `check` or
  `dependency_preparation` only. `task-runtime-evidence-v1.json`, and the `af/task-inspection`
  schemas that embed it, are narrowed to match, so runtime evidence an earlier release recorded no
  longer decodes.
- Self-optimizer contracts are narrowed in place. `af/OptimizationEconomics@1` drops
  `cache_results`, a per-kind `hit`/`miss`/`unknown` map that duplicated `cache_economics`: read
  `cache_economics.<kind>.hits`, `misses` and `unknown_results` instead.
  `af/OptimizationResult@1` and `af/OptimizationReport@1` drop the constant
  `live_demonstrations: "pending"` field, and optimize Task requirements no longer carry it; the
  Markdown report replaces its "Milestone gates" section with one plain sentence saying live paid
  demonstrations and adoption observations are still pending. A result `conclusion` is
  `validated`, `rejected` or `recommendation_only` (never `inconclusive` or `no_change`), and an
  `af/OptimizationVerification@1` `profile` is always `candidate`. The `af/ExperimentalSlot@1` and
  `af/ExperimentTrialResult@1` contracts and their `experimental-slot-v1.json` and
  `experiment-trial-result-v1.json` schemas are gone. An experiment arm Worker that declares an
  `execution_configuration` input is now refused like any other unavailable input instead of
  having it silently dropped. Optimizer artifacts an earlier release stored may no longer decode,
  and native observations that older captures stored under source-dependent IDs are no longer
  merged, so replaying such a history can count them twice.
- Published schemas are narrowed. `campaign-manifest-v1.json` now requires
  `check_timeout_seconds` and `git_timeout_seconds`, which every current Campaign manifest already
  carries; a manifest from a release that predates them no longer loads. The schemas also drop
  values that no release ever wrote. A dirty `SourceSnapshot@1` capture `boundary` is always
  `revalidated` (`filesystem_snapshot` is gone), and a Gate Execution Binding's
  `provided_isolation` in `run-report-v6.json` and `run-event-v1.json` is `none` or `container`
  (`process` is gone). In `task-contracts-v1.json`, and every schema that
  embeds its Task phase or result, a Task phase is never `resolving`, `planning` or `verifying`, a
  Task result's `execution` is `completed`, `incomplete` or `exhausted` (never `blocked` or
  `cancelled`), and the unreferenced `reviewConclusion` definition is gone.
- A Codex Task Worker's reply is read only from the `-o` last-message file that `codex exec`
  writes (codex-cli 0.147.0 always writes it). The Task adapter no longer falls back to the last
  `agent_message` event on stdout, so with a codex CLI that does not write that file the Attempt
  fails with "Codex Worker returned no final message"; the usage it reported is still charged.
- Source Manifests have one path spelling. The `path_encoding` field (`legacy_v1` or
  `percent_v2`) is gone: every path is spelled the way capture already spelled new trees, so a
  path that starts or ends with whitespace, or holds a space together with a `%` or non-UTF-8
  bytes, is percent-escaped (a leading space, as in `" notes.md"`, is percent-escaped to
  `%20notes.md`, and `a%b c` becomes `a%25b%20c`). That now
  includes a file a reviewer or Worker creates during a run, which a sandbox seal, warm workspace
  scan or Task delivery spelled literally when the baseline was an ordinary tree. A Snapshot's
  content digest hashes the stored spelling, so ordinary trees keep their digests, but a tree
  with such a path gets a different Snapshot digest than an earlier release gave it. ADR-0024 is
  superseded by ADR-0113 and removed.
- `af` builds and runs on Linux and macOS only. A source build for any other host, including
  Windows and the BSDs, now stops with a compile error. It no longer compiles fallbacks that
  skipped read-only sandboxes, process-group kills, symlinks or executable bits. The release
  targets and `install.sh` are unchanged.
- `af review report` no longer has a `spend` section: the JSON of `af/review-report@4` drops the
  `spend` array (the schema no longer lists it), text output drops its `Spend:` block, and
  Markdown drops its `## Spend` table
  and `### Attempts` list. They described only Attempts of the pre-Task executor, so for a Round
  a Task hosts they were empty or held a zero-token placeholder row; `task_accounting` reports
  those Rounds' Attempts, usage, wall-clock and caps. `RunReport@1` and `RunReport@2` events
  are neither written nor read, so a Campaign whose log holds one can no longer be run, reported
  or listed. The unused `run-report-v2.json` and `review-report-v2.json` schemas are gone.
- The retired shell review harness is gone from the repository: `compat/legacy-harness/`, the
  `fixtures/synthetic/` corpus generated from it, and the `fixtures/legacy/` private-corpus
  tests. `make fixtures` and `make review-kernel-test-corpus` no longer exist, and `make check`
  no longer regenerates the corpus. A Campaign event log that holds an artifact-less
  `FindingReported@1` (the `"imported": true` shape that only the unused `ledger.jsonl` importer
  wrote) no longer replays, and `af review ledger`, `af review show` and `af review report` no
  longer print an "unavailable: legacy import" placeholder.
- `af review run` and `af provider doctor` run every Campaign on the common Task runtime; the
  pre-Task executor they fell back to is gone. A Campaign whose log holds events only that
  executor wrote (`RunReport@3` to `@5`, Cold Closeout or Session Snapshot events, from Rounds run
  by af 0.9.0-rc.0 or earlier) is refused with "Campaign predates the common Task runtime
  (af < 0.9); start a new Campaign"; one that holds its reviewer Attempt, Provider Operation or
  broker events no longer replays at all (see below). A Campaign whose first Task capture failed
  now retries capture on the common runtime, including after `--restart-round`,
  `af review policy-time advance`, `af review evidence add` or `af review demand waive`, where it
  used to run on the pre-Task executor. `--resume-provider` is gone and is now a usage error
  (exit 2); it only continued that executor's fenced Provider Operations, and the common path
  already refused it. The `af/review-outcome@1` and `af/provider-doctor@1` documents, which only
  that executor printed, are no longer produced: Providers are admitted by the Review Task's own
  probe Attempts, and doctor prints `af/provider-doctor@2`. `af review run` no longer requires
  `HOME` up front. `af review report`, `ledger` and `campaigns` count only Task Attempts toward a
  Campaign's wall-clock, so a pre-Task Campaign's report no longer shows one. ADR-0016 is
  superseded by ADR-0113 and removed. A `--restart-round` before the Task exists now keeps
  Round 1's original prior Finding Set even when the candidate changed, so the Task captured on
  the new epoch resumes; each later run used to fail with "restarted Review changed its original
  prior sets or adjacent epoch".
- The pre-Task executor's Campaign event types are gone from the event vocabulary and from
  `run-event-v1.json`: `AttemptAdmitted@1`, `AttemptDispatched@1`, `AttemptFailed@1`,
  `AttemptFenced@1`, `AttemptFeedback@1`, `AttemptInput@1`, `AttemptReleased@1`,
  `ReviewerExecutionBound@1`, `BrokerOperationCompleted@1` and `ProviderOperationTransition@1`,
  with the `provider-operation-transition-v1.json` schema and the `review.kernel/RefusalHistory@1`
  artifact type. No current command wrote them. A Campaign log that holds one fails to replay with
  "unknown review-kernel event type: <type>; this log was written by another af release; start a
  new Campaign or Task", so `af review run`, `report`, `ledger` and `show` fail on it, and
  `af review campaigns` lists it as a problem. Every event log that holds an event type this
  release does not know fails with the same message. `af review report` no longer carries the
  optional `recorded_not_gathered` field, or prints its "Recorded, not gathered" section, which
  only such events filled; the field is gone from `review-report-v4.json`.
  `af review run` still lists recorded, not gathered results from the Round's Task Attempts. The
  `af review ledger` notice for an absent latest-Round Ledger drops its
  "(N admitted result(s) remain recorded, not gathered)" clause, which always counted 0.
  ADR-0022 and ADR-0023 are superseded by ADR-0113 and removed.
- `RunReport@3`, `@4` and `@5`, the run conclusions only the pre-Task executor wrote, are gone
  from the event vocabulary and `run-event-v1.json`, with the `run-report-v3.json`, `-v4.json`
  and `-v5.json` schemas. `RunReport@6` is the only run conclusion. `run-report-v6.json` now
  defines its outcome, verdict, binding and cache shapes itself, and
  `task-review-gate-facts-v1.json` takes its Cache failure shape from it. A Campaign log that
  holds a retired report, `RunReport@1` to `@5`, no longer replays. When that report is the
  first record replay cannot read, `af review run` and `af provider doctor` refuse the Campaign
  with "Campaign predates the common Task runtime (af < 0.9); start a new Campaign"; when a
  retired Attempt, Provider Operation or broker event comes first, they print the unknown event
  type message above. `af review report`, `ledger` and `show` fail on it, `af review campaigns`
  lists it as a problem, and a new event for the Round such a report concluded is refused with
  the unknown event type message. The Round rows of `af review report` drop `reported_tokens`,
  which only those reports' plain numeric spend filled. Every row now carries
  `task_chargeable_tokens_at_report` and `task_accounting`, which `review-report-v4.json`
  requires.
- The `gate_blocked` suppression reason is gone; only the pre-Task executor's scheduler wrote it.
  A Review Gate is a Task condition, so a node behind a Gate that did not pass reads
  `branch_not_selected` in `af/TaskRunReport@2` and in `af/review-outcome@3` node
  outcomes, or `upstream_missing` once its predecessors were suppressed, and `RunReport@6`
  records both as `upstream_missing`, as before. `task-run-report-v2.json`, `run-report-v6.json`
  and `review-outcome-v3.json` no longer list `gate_blocked`, and the
  review-outcome `ledger_production` no longer lists `not_produced_gate_blocked`. A stored report
  that carries `gate_blocked` no longer decodes.
- A Campaign manifest records only the canonical `report-derived@1` Finding identity policy, and
  `campaign-manifest-v1.json` no longer lists `legacy-path-title@1`. A Campaign whose manifest
  pins that path/title policy (opened before path-independent Finding identity, ADR-0006) can no
  longer be run or continued: its manifest is refused for an unknown finding identity policy, and
  its Ledger reports the manifest as unavailable authority. The Ledger reads a Report only as an
  enveloped `FindingReport@1` whose locations are canonical repository paths. An un-enveloped
  Report, the flat pre-`FindingReport@1` shape, or a Report with a noncanonical location such as
  `./src/a.rs` now projects as an unreadable-authority placeholder that blocks convergence; a
  noncanonical location used to leave the claim readable with unknown Scope. A Ledger node's
  `FindingSet@1` output must be an envelope: the untyped `{round, sources, findings}` summary is
  refused.
- `af review report --json` Finding objects no longer carry `news_round`, a Round counter no
  decision read; convergence counts news by `scoped_news_round`, as before. A Finding view's ID
  hashes the view, so view IDs differ from the ones an earlier release computed for the same
  Finding. A `FindingReport@1` relation only `corroborates` a `finding`: the `disputes` kind and
  the `report` target, which no release wrote, are gone from `finding-report-v1.json`, and a
  Report that uses them no longer decodes.
- Review pipeline format 1 is gone. A pipeline that declares `version = 1`, the format without
  `[subject]` whose untyped `findings`, `prior_findings` and `change_set` ports were typed by their
  names, is refused as an unsupported version; formats 2 through 5 still load (see the typed-port
  entry below). Declare
  `version = 2` with `[subject]` and typed `FindingSet@1` or `ChangeSet@1` ports instead. A
  Campaign whose manifest pinned a format 1 pipeline can no longer be resumed or continued.
- Campaign review has one reviewer contract, `review.kernel/ReviewerResult@2`. A pipeline is
  refused when it loads if a reviewer declares `review.kernel/ReviewerResult@1` or an untyped
  result output such as `outputs = ["result"]`, or if its Generation emits
  `review.kernel/PriorFindings@1`. Every reviewer and every Scatter must declare one optional,
  singular `review.kernel/FindingSet@1` input with snapshot affinity `any`, wired from
  Generation's `FindingSet@1` output. A reviewer without it used to run as `ReviewerResult@1`,
  and a Scatter without it fell back to `@1` silently; both are now refused at plan time.
  Reviewers answer `dispositions`, one per assigned prior Finding (`corroborate`,
  `not_reproduced` or `dispute`, keyed by `finding_id`), instead of `disputes` keyed by
  `claim_id` with `confirm` or `refute`. `af onboard` and the software starter already write this
  wiring; update a hand-written pipeline and its pin in `.af/af.lock`. The
  `reviewer-result-v1.json` schema and its conformance corpus are gone, and
  `reviewer-result-v2.json` now defines the flat report shape itself. `finding-set-v1.json` accepts
  only `review.kernel/finding-reducer@2` and `task-review-result-metadata-v1.json` only
  `ReviewerResult@2`, so a stored `ReviewerResult@1` result or a Finding Set reduced by
  `finding-reducer@1` no longer loads; a Ledger that reduces no reviewer result now records
  `finding-reducer@2` as well, so that Finding Set's ID differs from an earlier release's. A
  command reviewer's stdin document now always carries
  `"result_contract": "review.kernel/ReviewerResult@2"`.
- `review.kernel/ReviewerResult@2` no longer has the reviewer's `verdict` and `summary`, which no
  decision, report or display read. A result is exactly `reports`, `benchmark_demands` and
  `dispositions`, and the model output contract no longer asks for the other two. A model reviewer
  that still sends them is unaffected, because its answer is normalized to the contract. A command
  or Task reviewer Worker whose reply still carries them is refused as
  `unexpected_or_missing_fields`: drop both keys from its output and from the package's
  `outputs/result.schema.json`, then re-pin the package digest. That includes the `bugs` and
  `correctness` Workers of a starter an earlier `af catalog init --profile software` wrote; run
  it again into a new directory to get the current ones.
- Every review pipeline port is a typed table, in every pipeline format. The string shorthand
  (`outputs = ["decision"]`, `inputs = ["reports"]`) is refused when the pipeline is parsed, and so
  is a node without `outputs`, which used to get an implicit `out` port. A Gate, Gather or Ledger
  output typed `review.kernel/Opaque@1`, the type the shorthand stood for, is no longer retyped by
  its node kind: it is refused as an unsupported Review output before the Round's Gate runs, and
  the Store no longer skips payload validation for Opaque@1 artifacts. Spell each port out as
  `{ name = "…", type = "…", cardinality = "one", optional = false, snapshot_affinity = "any" }`:
  a Gate outputs `review.kernel/GateDecision@1`, a Gather `review.kernel/ReportSet@1`, and a
  Ledger exactly one `review.kernel/FindingSet@1`, optionally beside a `review.kernel/DemandSet@1`;
  a Ledger without that one Finding Set output is refused when the pipeline loads, where a lone
  untyped Ledger output used to receive the Finding Set by position. `af onboard` and every
  shipped pipeline already write typed ports. A Campaign whose manifest pinned a pipeline with the
  shorthand can no longer be resumed or continued.
- Brokered credentials are gone; GA has no broker. A format 4 or 5 reviewer's `execution` accepts
  only `credential_mode = "credential_free"` or `"trusted_unsafe"` and `auto_apply`:
  `credential_mode = "brokered"` and an `operations` list are refused when the pipeline is parsed,
  in `af review plan` too. They used to pass `plan` and fail only at `af review run`, because no
  adapter could serve them. No shipped pipeline declares either. Provider admission always
  compiles to `af/TaskProviderAdmission@1` with the `af/TaskProviderContext@1` readiness context.
  The captured `af/LegacyReviewTaskPolicy@4` drops `settings.provider_probes`, which was always
  empty, and holds the review settings directly under `settings`, so a new Review Task's policy
  digest differs from an earlier release's. The `task-provider-admission-v2.json`,
  `task-provider-context-v2.json` and `task-provider-probe-policy-v1.json` schemas are deleted, and
  `compiled-task-v1.json` loses the `provider_admission_brokered` operator.
- The Broker's Task records went with the Broker; no release wrote them, because none ever
  installed a Broker. `TaskBrokerTransition@1` is no longer an event type, so a Task log holding
  one fails to replay. `af task show --json` and `af task explain --json` no longer emit
  `af/task-inspection@4`, a `broker_records` section or a `broker_transition` history entry, and an
  `af self optimize` history source with the `af` adapter refuses an `af/task-inspection@4`
  receipt. The `task-broker-binding-v1.json`, `task-broker-operation-v1.json`,
  `task-broker-transition-v1.json`, `broker-operation-receipt-v2.json` and
  `task-inspection-v4.json` schemas are deleted, and `run-event-v1.json` and
  `task-inspection-v11.json` drop their Broker entries.
- Task inspection has one version. Every command that prints a Task as JSON (`af task show`,
  `explain`, `plan`, `start`, `run`, `approve` and the others, and `af self optimize`) now emits
  `af/task-inspection@11`, instead of a version from `@3` to `@11` chosen by the sections the Task
  happened to have. Each section beyond the core (`owned_child_sets`, `review_handoffs`,
  `review_integrations`, `attempt_walls` with `runtime_observations`, `experiments` and
  `adoption_observations`) appears only when the Task recorded it, and `history` holds
  `TaskTransition@5` payloads. `task-inspection-v11.json` is now self-contained and
  describes all of it; it no longer requires `experiments` and `adoption_observations`, and a
  Failed settlement's `diagnostic` must be a JSON object, as this release always writes it. The
  `task-inspection-v3.json` and `-v5.json` to `-v10.json` schemas are deleted, and
  `task-list-entry-v2.json` and `task-plan-inspection-v1.json` now refer to
  `urn:af:schema:task-inspection:11`. The `af` history adapter of `af self optimize` accepts only
  `af/task-inspection@11` receipts, so a receipt an earlier release exported is refused. A script,
  skill or hub check that matches another `af/task-inspection@N`, or validates against a deleted
  schema, must switch to `@11`.
- Task usage and Review provenance have one encoding each. Every usage artifact is
  `af/TaskTokenUsage@3` and every Task Review provenance artifact is
  `af/TaskReviewAttemptProvenance@2`, whatever the width of their counters; narrow values no
  longer select `af/TaskTokenUsage@1` or `@2` or `af/TaskReviewAttemptProvenance@1`. The
  payload bytes are unchanged, but the artifact types and content IDs of new usage and
  provenance artifacts differ from those an earlier release wrote. A Task Review selection whose
  provenance or usage artifact carries a retired type, or no artifact envelope at all, is
  refused, and the `task-token-usage-v1.json`, `task-token-usage-v2.json` and
  `task-review-attempt-provenance-v1.json` schemas are deleted: every other schema now refers to
  `urn:af:schema:task-token-usage:3` for its decimal counters.
- Task records have one version per name. `TaskTransition@1` to `@5` collapse to
  `TaskTransition@5`, `af/TaskExecutionRecord@1`, `@3`, `@4` and `@5` to
  `af/TaskExecutionRecord@5`, `af/TaskRunReport@1` and `@2` to `af/TaskRunReport@2`, and
  `af/TaskReviewHandoff@1` and `@2` to `af/TaskReviewHandoff@2`. Every change kind, record kind
  and field is kept: the new versions are supersets of the old ones, so a transition now carries
  `review_continued`, `review_integration_selected`, `review_integration_finished`,
  `recording_resumed` or `adoption_observation_recorded` under the same number as `opened` or
  `finished`; a settlement or usage observation carries its decimal-text `charged_tokens`, and
  the owned-child and experiment kinds travel in the same record type. An Integration phase
  report is `af/TaskRunReport@2` with a `phase_id`; a Round report is the same type without one.
  The `task-transition-v1.json` to `-v4.json`, `task-execution-record-v1.json`, `-v3.json` and
  `-v4.json`, `task-run-report-v1.json` and `task-review-handoff-v1.json` schemas are deleted,
  and `run-event-v1.json` and `task-inspection-v11.json` name only the surviving versions. This
  is a wire break: a `.af/state` Task log or CAS record an earlier release wrote no longer
  decodes, and `af task list` and `af self optimize` fail for the whole store while one remains,
  rather than skipping it. Finish or delete in-flight Tasks before upgrading. The
  `execution_records[].artifact_type` and `review_handoffs[].artifact_type` values in
  `af task show --json` each collapse to one string.
- A Task catalog and its captured run authority have one schema each: `af.task-catalog/2` and
  `af.task-run-authority/2`. `provider_admission` is now optional in a catalog; omitting it means
  the fixed 4,096-token, 45-second admission allowance that `af.task-catalog/1` had, and
  declaring it keeps the explicit bounded cost. A committed `.af/task-catalog.toml` that still
  says `schema = "af.task-catalog/1"` is refused with `Task catalog requires schema
  af.task-catalog/2`; change that one line by hand (nothing else about the file changes).
  `af catalog init` now writes `af.task-catalog/2` for every profile. The captured run
  authority always records the resulting admission cost and re-checks it against the captured
  catalog bytes, and `task-catalog-v1.json` is deleted.
- The Attempt wall sidecar in `events.sqlite` keeps one exact usage column, `usage_v3_json`,
  beside `usage_observation_v1_json`; both are created with the `attempt_wall` table. The numeric
  token columns, `usage_v1_json`, `usage_v2_json` and the `ALTER TABLE` migration that ran on
  every open are gone, as is the Campaign-keyed narrowing view only the removed pre-Task executor
  wrote. Wall rows an earlier release recorded in the retired columns are no longer read, so
  finish in-flight Tasks before upgrading: recovery raises an abandoned Attempt's charge from its
  recorded wall usage, and without it the Attempt settles at its reservation floor.
- `af review run --json` always emits `af/review-outcome@3`, instead of `@2` when every selected
  Attempt's usage fitted u64, and `review-outcome-v2.json` is deleted.
- `af review report --json` always emits `af/review-report@4`, instead of `@3` for narrow usage or
  `@1` for a Campaign with no Task. `task_accounting` is always present; it is empty exactly for a
  Campaign whose first Task capture failed, which has no Rounds either, and
  `review-report-v4.json` describes that case. `review-report-v3.json` is deleted. A script, skill
  or hub check that matches `af/review-outcome@2`, `af/review-report@1` or `af/review-report@3`,
  or validates against a deleted schema, must switch to the single version.
- The reviewer output contract a model Worker is prompted with now names its report array
  `reports`, the key the stored `ReviewerResult@2` artifact and every Worker package schema
  already used, instead of `findings`. An answer that still says `findings` is read as `reports`,
  so hand-written command reviewers keep working, but the prompt bytes changed: reviewer Attempt
  context IDs differ from those an earlier release computed for the same node.
- This file starts at the first GA release: the `0.7.0`–`0.9.0-rc.6` sections are out of it and
  stay on the GitHub release pages and in git history. `README.md` and `SECURITY.md` now state
  the GA policy — compatibility obligations start at 1.0, and only the latest `1.x` minor line
  receives security fixes.

### Changes

- Make Provider onboarding safe for automated callers (ADR-0112): refuse to start an official CLI
  login unless the operator opted in with `--login` *and* the process owns an interactive terminal
  on stdin, stdout and stderr, returning the `human_action_required` result with the exact
  private-terminal command instead; warn about OAuth URLs and authorization codes before handing
  over the terminal; keep registering an already-authenticated context with no login at all; add
  stable versioned `af/provider-status@1` and `af/provider-setup@1` documents under `--json` that
  distinguish registration, authentication, usable-or-untested and usage without exposing account
  email, organization identity, credentials, OAuth material or raw Provider output; make
  `af provider status` a fast registry and authentication check with subscription and quota probes
  behind `--usage`, where an unavailable probe exits 7 and leaves an authenticated Provider
  authenticated; document exit codes 3 (human action required), 4 (Provider CLI missing), 5
  (registry conflict), 6 (authentication failed) and 7 (usage unavailable) with results on stdout
  and diagnostics on stderr; and keep `af provider doctor` the charged end-to-end usability check.
- Remove the `task_planning` integration-test timing race: a Task deadline is absolute
  wall-clock from creation, and the generated-plan tests hold one Task open across a long chain
  of `af` invocations, Git commits, catalog operations, signing and Python Workers. Under the
  four-thread full test gate those subprocesses consumed the fixtures' 60s budget, so valid
  generated-plan and imported-catalog resumes were correctly but unhelpfully refused with `Task
  deadline protects still-required verification`. The long-lived Tasks in
  `crates/af/tests/task_planning.rs`, `task_catalog.rs` and the native-model cases in `task_file.rs`
  now use a documented ten-minute wall budget, added to the total and taken from nothing:
  per-Attempt walls, Attempt counts, token budgets and the verification reserve keep their fixture
  values, and no production code changed. New tests pin both halves — only the total fixture wall
  moves, and `TaskBudget::prepare` keeps its exact deadline boundary and refusal wording at both a
  small and a large budget
  ([ADR-0114](docs/adr/0114-budget-cli-task-fixtures-for-loaded-machines.md)).
- The internal delivery records are out of `docs/`: the `P00`–`P14` package checklist, the product
  backlog, the release-timing and validation-cost measurement records, the self-optimizer plan and
  review record, and the pre-implementation design notes the shipped architecture was ported from
  (`docs/design/{overview,entities,state-machines,config,store,research,task-execution,task-execution-examples}.md`).
  Git history keeps them. The engineering values are now [`docs/values.md`](docs/values.md), and
  `docs/design/` keeps only the two designs still in flight, warm layers and the self-optimizer.
  `CONTRIBUTING.md` now documents the opt-in `make check TEST_RUNNER=nextest` runner.
