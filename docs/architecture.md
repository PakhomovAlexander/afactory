# Architecture

How Afactory's Task runtime, Review Kernel, Scenarios and Projects are built, and why each
boundary sits where it does.
This is the long-form companion to the [README](../README.md): the crate layout, the contracts,
the store, source capture, Check nodes, reviewer adapters, sandboxes, the pipeline graph, pipeline
definitions, routing, and Attempt budgets. The vocabulary it uses is defined in
[`CONTEXT.md`](../CONTEXT.md); the decisions it rests on are recorded as ADRs under
[`adr/`](adr/README.md); the values that order its trade-offs are in [`values.md`](values.md).

Behavioral sections name the tests that pin their boundaries; crate manifests and source
inventories record the implementation structure.

## Layout

```text
schemas/     the language-neutral contracts (JSON Schema 2020-12)
crates/
  af-contracts/   shared Task, Snapshot, Worker and Review contracts,
                 plus the flat reviewer-result reader
  af-parallel/ bounded executor shared by
                 filesystem-heavy infrastructure
  af-process/ bounded subprocess supervision shared by source capture,
                 reviewers, checks, and sandbox providers
  af-store/  canonical identity, the artifact CAS, the append-only log,
                 and the rebuildable Findings Ledger projection
  af-source-git/  offline, read-only capture of a tree as an immutable
                 content-identified snapshot, and materialization into a sandbox
  af-check/  typed Check nodes: trusted programs with typed argument slots,
                 per-attempt CheckResult records, and a gate that never passes
                 what it did not verify
  af-runner/ Worker transport: command and model runners behind one
                 contract, plus shared model/effort validation and binding diagnostics
  af-sandbox/ materialized sandboxes, sealed mutation capture, and an
                 isolation level a pipeline can refuse
  af-graph/  the typed pipeline: named ports, deterministic planning, and
                 gates compiled into Task conditions
  af-runtime/ common Task execution: admission, recovery, acceptance and accounting,
                 plus Review composition over capture, sandboxes, checks and the ledger
  af-project/ Project identity and Stores, registered Workspaces, Surveys and Brief services
  af-pipeline-config/ pipeline definitions, validation and captured authority
  af-attempt/ attempt fencing and budgets that reserve before they spend
  af-runner-claude/, af-runner-codex/, af-runner-pi/  the model adapters:
                 digest-pinned Worker packages driving `claude -p`, `codex exec`
                 and Pi's structured host protocol
  af-source-issue/ read-only capture of external issue sources into typed data
  af-scenario/ Scenario contracts, durable Runs, Ant scheduling and admission, and
                 bounded orchestration through explicit application adapters
  af-telemetry/ Telemetry's leaf crate: segments and their consumer thread, blobs,
                 structured redaction (the credential rules af-runner re-exports), the
                 tracing subscriber, the rebuildable index, the run views and the telemetry
                 kind of the Storage Budget; never read by the kernel (ADR-0153)
  af/            CLI presentation and application services: onboarding, Review, Tasks,
                 Scenario/Ant and Project Host adapters, machine policy, Providers,
                 storage and self-management
fixtures/
  self-optimizer/ the credential-free Worker catalog and native examples the optimizer uses
  task-contracts/ the additive Task contract corpus
  task-runtime/  Task runtime scenarios (review, embedded review, bounded repair)
```

Library package, directory and Rust import names follow their shared AF responsibilities
([naming decision](adr/2026-11-06-name-shared-libraries-for-af-responsibilities.md)). The `af`
package and binary retain their names. These implementation names do not rename the Review
Kernel's domain terms, persisted `review.kernel/*` or `af/*` identities, schema IDs or CLI commands.

The dependency direction is from application services toward shared libraries. This map groups
the principal workspace dependencies; each crate's `Cargo.toml` lists its direct edges.

| Boundary | Responsibility and dependencies |
| --- | --- |
| `af-contracts`, `af-process`, `af-parallel`, `af-telemetry` | Independent leaves: shared typed evidence, subprocess supervision, bounded filesystem work and observational telemetry. None depends on another AF crate. |
| `af-attempt`, `af-graph` | Attempts depend on contracts; graph planning depends on contracts and Attempts. Neither executes Workers. |
| `af-store`, `af-source-git`, `af-check`, `af-sandbox`, `af-runner` | Storage and execution infrastructure built on shared leaves; source capture, checks, sandboxes and runners retain their own boundaries. |
| `af-pipeline-config` | Parses and validates definitions, packages, pins and authority using contracts, graph and infrastructure. It has no runtime dependency. |
| `af-runtime`, Provider adapters | Runtime depends on config, graph and infrastructure and receives Worker adapters. Claude, Codex and Pi depend on shared runner contracts and storage, with no dependency on runtime or each other. |
| `af-scenario`, `af-project` | Scenario depends on contracts, Store, process and sandbox; Project also uses config, graph, runtime, runner and source capture. Each receives application services through its own `Host`; neither depends on `af`. |
| `af` | Composes the libraries, implements their Host interfaces, owns machine policy and Provider bindings, and presents the CLI/TUI. `af-source-issue` supplies bounded issue capture. |

`af_runner::models` owns model/effort validation for Claude, Codex and Pi, including
catalog resolution, and `models::diagnostic` owns grammar G and binding-refusal wording.
Adapters and neutral consumers (Provider validation, planning, onboarding and TUI) use this common
API; Codex has no dependency on the Claude adapter. Claude retains native refusal, model-switch
and unavailable-model handling, including its diagnostics in `af_runner_claude::hint`.
The common `models::tests` and `models::diagnostic::tests` preserve validation and printed words;
Claude's `hint::tests` and `task_model_fallback` tests preserve native outcome diagnostics and
hint extraction. See the [ownership decision](adr/2026-11-06-share-model-validation-outside-provider-adapters.md).

Onboarding emits current Task catalog packages, including a standalone native Review Task file.
Onboarding validation and machine doctor share `task_commands::onboarding`: bounded no-follow
package capture, digest checks and the actual Task authority compiler. They neither read retired
Review Definitions nor dispatch Workers. Archived Proposal export replays current-format Task
log prefixes; the current authoritative head follows validated durable Integration commit records.
The Tasks pane and `af review ledger` select the complete canonical Finding Set, including
unresolved earlier Findings. Ledger JSON additionally exposes the original Report envelopes;
`af task findings` remains the separate current-Round report view.

Within `af`, `project_policy` parses `.af/af.toml`; `af_project` owns Projects, their Stores,
registered Workspaces, identity-bound creation/removal, Surveys and `af_project::brief`.
The library's `Host` injects state roots, default Task Store paths, path resolution, machine Store
registration, declaration validation and effective policy tables; `af::project::Application` wires
these to the existing machine services. The configuration ladder and the full `project_policy`
parser stay in `af`, while the bounded path-pattern grammar shared by routing and Surveys lives in
`af_pipeline_config::path_glob`. Isolated worktree Git construction is shared by Project operations
and Task delivery through `af_project::worktree_git`; `af_process` still owns supervision.
`crates/af/src/project/` retains clap dispatch, JSON/text presentation and concrete Task adapters.
The library's Brief resolver reads captured authority only; live capture receives its Host explicitly,
and receipt assembly receives the existing recorded-input-bindings reader from the application.
Domain tests live under `crates/af-project/src/`; CLI integration tests stay in
`crates/af/tests/it/project*.rs`. The two `environment::tests` pin explicit state/registry/declaration
and effective-policy dependencies. See the
[Project boundary decision](adr/2026-11-06-separate-project-services-from-cli.md).

`task_commands` captures authority and handles the Task CLI lifecycle; `task_delivery` inspects
results and delivers verified candidates. The actual `TaskRuntime` remains in `af_runtime::task`.
`af_store::store::task::review_integration::native_review_conclusion` joins native Review evidence
with the exact common Task log prefix and cumulative accounting; `af_runtime::task::review` assembles
and validates the result. `self_management` handles installed releases. The native
`task_review::native::native_conclusions_freeze_exact_wide_usage_and_late_usage_fences_the_old_result`
regression proves the accounting join, including late usage and cold replay. Project, Brief and
Task delivery tests exercise their corresponding boundaries.

`af_scenario` owns Scenario/Ant contracts, pins, journal replay, scheduling, roadmap admission
and execution orchestration. Its `Host` interface injects native Task validation, planning,
receipts, cancellation and accounting, plus machine Store locations, ownership, registration and
collection holds. `crates/af/src/scenario.rs` retains clap parsing and terminal/JSON
presentation; `crates/af/src/scenario/host.rs` implements native Task and Storage calls using
existing application services. The library retains authority, signature, bounds, recovery and
publication controls, and never depends on the binary. Domain tests live in `crates/af-scenario/src/`; Scenario CLI
integration tests remain in `crates/af/tests/it/scenario*`. The library tests
`native_template_validation_is_required_at_admission_and_every_journal_replay` and
`recovered_host_accounting_retains_unknown_reservations_exact_overruns_and_monotonicity` pin the
injected validation and accounting boundaries; the CLI host test
`native_template_host_keeps_the_task_file_parser_and_binding_refusals` retains native grammar
coverage, including the shipped templates. See the
[boundary decision](adr/2026-11-06-separate-scenario-domain-from-cli.md).

The Task `run_report` modules in `af-contracts` and `af-runtime` describe and assemble run
outcomes; `af_contracts::task::execution_summary` defines the `af task report` document, and the
Task `report_domain` modules own Report Task contracts and execution. `warm_workspace` in
`af-contracts` and `af-sandbox` names Campaign template roots, distinct from the registered
Project Workspaces in `af_project`. The existing report schema parity and workspace tests preserve
those contracts and both workspace ID algorithms.

## Worker local development

A captured Task command Worker can opt into a separately versioned `development.toml` companion.
Its existing package pin binds the requested exact machine environment. The container provider,
CheckRuntime lifecycle and process supervisor enforce private roots and supported bounds; the
host retains command evidence on the existing Attempt. Reviewer experiments use a second clone
and cannot enter the candidate seal. Model adapters and unsupported quota requests fail closed.
Declared cache policies select exact immutable layers in `WarmSet@3` before reservation;
private clones and admitted same-slot `BuildCache@2` outputs support explicit continuation.
Approved dependency sources remain available-only, and Worker caches have no Gate authority.
A separate pinned `knowledge.toml` companion selects bounded manifests and on-demand source
excerpts through typed ports on both model and command Workers. Private Notes remain same-slot
hypotheses with source freshness checks and existing Warm Set/Head Delta evidence. Indexes are
immutable derived records in the Task CAS; the recorded invocation pins selection across retry
and restart. Cross-Task source facts require an explicit capability and content revalidation.
Sessions remain unsupported. The [design](design/worker-local-development.md) and
[knowledge example](../fixtures/worker-knowledge/README.md) explain the protocol and evidence;
[development examples](../fixtures/worker-development/README.md) show Rust and Python using the
same runtime. `task::development::tests` proves transport, state,
provenance and experiment isolation; the existing live container probes own containment proof.

## Contracts

| Schema | Carries |
|---|---|
| `artifact-envelope-v1.json` | type, content and artifact IDs, producer, exact inputs, subject snapshot |
| `finding-report-v1.json` | one immutable claim by one attempt — no status, no round, no resolution |
| `finding-set-v1.json` | one Subject-bound canonical Finding view at a ledger barrier |
| `source-snapshot-v1.json` | content identity, never a branch; committed, synthetic-worktree or derived |
| `patch-proposal-v1.json` | an atomic change set naming the exact claims it covers |
| `run-event-v3.json` | the append-only stream envelope; `sequence` orders, never `occurred_at` |
| `check-result-v1.json` | one execution of one check, per attempt — `not_run` is its own status; a remote result names its evidence instead of a command |
| `remote-check-evidence-v1.json` | every remote fact one Remote Check's decision used: refused, published or observed (ADR-0140) |
| `reviewer-result-v2.json` | what one reviewer attempt returned: reports, demands, and one disposition per assigned prior Finding |

The schemas are the contract; the Rust types are one view of them. `crates/af-contracts/tests/schema_parity.rs`
checks both directions — a populated value must validate, and a value the design forbids must be
rejected — because a schema that accepts everything passes a one-directional test.

Three things are deliberately unrepresentable rather than merely discouraged: a report has no
status field, a best-effort copy is not a `Capture` variant, and a patch proposal cannot name
zero claims.

`RunEvent@3` enumerates the closed current type vocabulary. Native Review authority is admitted
through checked Task publication; a generic Store append cannot manufacture a canonical Review
Round. Domain Finding and Demand records retain their shared canonical identities.

A `ReviewerResult@2` reviewer answers with flat `reports`; each one is converted to
`FindingReport@1` on its own and must satisfy it before any ingest admits it. `fix` is required,
because a claim with no proposed remedy is one a triager cannot act on. An empty `file`, or the
literal `(change-wide)` sentinel, is a change-wide claim and becomes an empty location list,
because the sentinel shares a namespace with real paths. One violation refuses every result at
that barrier, so a blocking Finding cannot degrade into an empty pass
(`crates/af-store/tests/it/canonical_identity.rs`).

## The store

Four layers, strictly one-directional — `canonical` -> `cas` -> `store` -> `ledger`:

- **`canonical`** — RFC 8785 canonical JSON and two domain-separated digests. `content_id`
  hashes the payload; `artifact_id` hashes the envelope. That split is what lets identical
  content be stored once while two provenance records stay distinct. Numbers outside the range
  it can format exactly are **refused**, not guessed at — a digest that is subtly wrong for
  large magnitudes is worse than one that fails loudly.
- **`cas`** — write temp, fsync, rename, fsync the directory. Raw payloads are addressed by
  `content_id`; typed envelopes are also stored and addressed directly by `artifact_id`, with both
  envelope and payload identity verified on read. A store that trusts filenames cannot detect
  corruption.
- **`store`** — SQLite in WAL mode, one writer, a dense per-run sequence that is the ordering
  authority (never `occurred_at`). It **refuses** an event referencing an artifact the CAS does
  not already hold, which turns a class of crash corruption into an immediate error. SQLite is
  what backs it today, not part of its interface: the interface stays backend-agnostic, and which
  database backs a later shared, consensus-based coordinator is deliberately undecided. Beside
  the log, outside it, the `store_writers` table names every af release that opened the Store
  for writing (`af/store-writer@1`, `schemas/store-writer-v1.json`): a release still refuses to
  replay a Store another one wrote (ADR-0145), and the refusal names that release and the fix,
  `AF_VERSION=<v> af …`. A Store with history and no marker was written before markers and is
  named as an older release's; no replay reads the table
  (`crates/af-store/src/store/writers.rs`, `crates/af/tests/it/store_release.rs`).
- **`ledger`** — the projection. `rebuild` is its only constructor, so hand-edited state has no
  way in.

Native Review retains the canonical identity policy, `report-derived@1`: a selected Report
artifact creates one path-independent Finding unless an explicit relation or exact trusted
occurrence key attaches it. Each barrier publishes an immutable, Subject-bound `FindingSet@1`.
The Ledger reads a Report only as an enveloped `FindingReport@1` whose locations are canonical
repository paths; any other Report artifact projects as an unreadable-authority placeholder that
blocks convergence.

### Replay and convergence

Current recovery belongs to `af-store/src/store/task/{journal,execution,lease}.rs` and the common
Task runtime. `af-runtime::it task_runtime::publication::domain_publication_recovers_after_restart_without_another_paid_attempt`
and `domain_invocation_publication_recovers_before_any_attempt_or_context` reopen the Store at
native publication boundaries and prove recovery without another execution. The native
`task_review::native::{native_integration_recovers_published_commit_without_repeating_proposals_or_checks,native_scatter_recovers_published_shards_without_another_child_attempt}`
regressions retain Review-specific commit/shard recovery. Store
`store::task::tests::wide_usage::settled_and_cumulative_attempt_usage_reopen_exactly_including_crash_recovery`
preserves exact charge across recovery; lease and reservation tests fence stale writers and
refuse invalid appends without consuming authority.

`crates/af-store/tests/it/ledger_convergence.rs` pins how the Ledger fold moves one Finding: a
fix that did not hold reopens, a higher-severity re-report escalates in place and is news, a
same-severity re-report is not, a rejected or `wontfix` claim is not reopened by a re-report,
`contested` blocks like `open`, a fix needs a clean Round before convergence, an open blocker at
the Round cap is `Exhausted` rather than a pass, and an open finding below the gate never
blocks. Nothing is lost on the way: both reports of a same-round duplicate stay attached with
distinct artifact IDs, and a reopen never overwrites the note the fix recorded.

Those scenarios set status with a bare-status `FindingResolved@1`, which a Review host writes
only as `contested`. Operator decisions are typed Resolutions, and they add one rule: an
in-scope re-report challenges a `rejected` or tracked-`wontfix` Resolution when its severity is
above what the Resolution accepted or its Subject is not the one the Resolution was decided on,
and the Finding becomes `contested` (`crates/af-store/tests/it/canonical_identity.rs`).

## The source adapter

A snapshot is identified by **content**: path, kind, executable bit, and the digest of the bytes.
Not by commit, branch, clone path, mtime or owner. The same tree in two unrelated repositories is
one snapshot, which is what makes "every reviewer inspected the same thing" a digest anyone can
recompute rather than an assumption.

Capture is offline and read-only, and the checkout is verified untouched afterwards. Two
providers, because the problem differs:

- **committed** — objects cannot change under the read, so one pass.
- **dirty** — there is no atomic read of a directory tree, so the boundary is *established*:
  fingerprint the index, take two complete passes, fingerprint the index again, admit only if all
  three agree, retry a bounded number of times, then fail closed. The failure it prevents is a
  torn tree — half the files from before an edit and half from after, digested as a state that
  never existed. A `CaptureObserver` seam lets the tests mutate the worktree *between* passes,
  which is the only honest way to show the boundary catches what it claims to.

### The hostile-configuration case

`crates/af-source-git/tests/it/hostile_git_config.rs` found a real hole on its first run.

The premise is that the repository *is* the attacker: `.git/config`, `.gitattributes` and hooks
are all candidate-controlled, and capture runs before any sandbox exists, with the operator's
privileges. The test plants hooks, a clean/smudge filter, a textconv, `core.fsmonitor`,
`core.autocrlf`, an alias, and a submodule URL pointing at a closed port — then asserts that no
marker file appears and that the digest equals a clean repository's digest for the same content.

It failed. `git status` hashes worktree files, and hashing runs the candidate's `clean` filter —
so the read-only *check* was executing attacker-chosen code. There is no configuration that
disables an in-tree filter driver by name, because the name is attacker-chosen. The fix is a
`SAFE_SUBCOMMANDS` allowlist enforced in the git adapter: only `ls-tree`, `cat-file`, `ls-files`,
`rev-parse` and `rev-list` may run, `git status`/`diff`/`add` are refused outright, and
`worktree_state` was rebuilt from the index plus a direct filesystem walk. A control test runs
plain `git status` over the same repository first, proving the marker *would* have fired.

The same premise holds wherever af runs git against a checkout, not only in capture. The
browser that bare `af` opens and the Task file advice run git through
`af_source_git::git::isolated_command`, the cleared environment and full `-c` override set
capture uses. The browser marks drift by comparing `ls-tree`/`cat-file` blobs with working-tree
bytes itself, never through `git diff` or `git hash-object`. Native Review resolves `--authority`, Base and candidate independently in
`af/src/task_commands.rs`, using the same isolated committed/worktree capture paths. The source
adapter's `af-source-git::it hostile_git_config::{filtering_subcommands_are_refused_outright,tree_diff_ignores_candidate_textconv_and_hostile_diff_configuration}`
regressions prove the Git boundary. `af::it tui::the_browser_runs_nothing_a_hostile_git_config_names`
covers browser reads with hostile filters and fsmonitor. Native selector safety is also exercised
by `task_file::native_review_preflight_commands_inspect_real_task_documents_and_refuse_empty_diff`:
planning and execution refuse an empty Diff before admitting a Task and name corrected selectors. A tree that names a path git itself refuses (`.git` in
any ASCII case or NTFS/HFS+ spelling, a symlinked
`.gitmodules`) is refused at capture, materialization and Task binding through one predicate,
`git_refuses_path`. Otherwise its `.git/config` would configure every `git` run in the sandbox.

## Check nodes

Two properties. `crates/af-check/tests/check_runner.rs` pins them against real processes;
the vacuous-gate rule is pinned by `a_vacuous_gate_blocks` in `crates/af-check/src/gate.rs`.

**Nothing overwrites.** A gate that runs five times in one round leaves five records: each
execution is its own immutable `CheckResult@1`. `check_runner.rs` runs one check twice and
asserts that both executions' artifacts survive: the failing attempt's stderr is a distinct CAS
artifact, still readable after the later attempt passes.

**A check that could not run is not a pass.** `not_run` is a first-class status carrying a
reason, and it blocks a required gate exactly as a failure does. So does a gate with no required
checks: a vacuous run is the most dangerous green there is, because it looks exactly like a clean
one and asserts nothing.

### Typed argument slots

A check command is not a string. A shell command line with `{tests}` filled from the change would
splice **paths taken from the diff under review** into it, and a file named `--config=/tmp/evil`
is one `git add` away.

So the program and every option are trusted literals from project configuration, and values
derived from the change are `untrusted`: no leading `-`, no `@response-file`, no embedded NUL.
The identical bytes are fine as a literal and refused as untrusted — the check declares where
such values go, and nothing else can put them there. Refusal rather than quoting, because
quoting depends on the program's own option syntax, which the kernel does not know and must not
guess. A refused command is `not_run` with the reason recorded; it never reads as "ran and
failed", because nothing was verified.

Elapsed time is deliberately absent from the record: no policy reads it, and its presence would
make an otherwise reproducible artifact differ on every run.

## Reviewer adapters, and why gather has a barrier

The `command` adapter comes first deliberately: its output is a function of its input, so every
scheduling property can be proved before a model is ever invoked. A model adapter is then a
different runner behind the same contract, with nothing above it changing.

Every way an invocation can go wrong is a typed outcome — refused before execution, unavailable,
failed, malformed output — because a reviewer that crashed and a reviewer that found nothing must
never be indistinguishable. One of those means the change was reviewed. Unparseable output is
stored before it is parsed, so "malformed" stays a falsifiable claim.

**Concurrency is fine; nondeterminism is not.** Reviewers finish in whatever order the machine
felt like, and the projection is order-dependent — a finding belongs to its *first* reporter, and
later ones become duplicates. Ingesting in completion order would therefore let scheduling decide
who owns a finding, and a replay would not reproduce the run it replays. So results are admitted
in canonical order (by node ID), never in arrival order.

Native `ReviewReduce` is that barrier. It validates all configured reviewer inputs against
one exact Subject, check set and prior history, then reduces them in canonical source order
through the shared Ledger. Missing siblings can be shown as recorded results but publish no
partial authoritative Ledger. `crates/af/tests/it/task_review.rs` exercises clean,
changes-requested, missing and unavailable reviewer cases through the native runtime. Reader-backed
native Review telemetry assertions live in that permitted CLI consumer; runtime tests link no
telemetry reader. Human Review decisions are deterministic Tasks of this same reducer, with exact
root inputs and no Workers, source changes or Integration. The decision signature replays the
selected complete Round, applies canonical domain transitions and publishes checked history through
the common Task lease. [Human Review decisions](task-execution/review-task.md#human-review-decisions)
describes approval, evidence and policy bounds.

### Seeing exactly what a Worker receives

`af review inspect TASK_ID --json` exposes the captured Task plan, exact invocation identities,
context manifests and selected outputs. Worker context is bound to the reserved Attempt and its
declared inputs; an inspection cannot manufacture Attempt authority. The old `review render`
frontend is retired. Model and command Workers use common Task request/reply contracts and the
same bounded runtime context admission.

### Command Workers

A command Worker runs in its materialized sandbox with the declared environment and a typed
`af.worker-request/1` on stdin. Its `af.worker-reply/1` must satisfy every declared output port's
schema and provenance rules. The common runtime seals the sandbox before admitting candidate
outputs. Command Review Workers use the same canonical flat Reviewer Result as model Workers
and need no model Provider; the native Review fixtures exercise this path without model tokens.

## Sandboxes, and what this provider is not

`trusted_local` is a materialized copy of a snapshot in a temporary directory, optionally
read-only. **It is not security isolation.** A process running as the same user can `chmod` its
way out of read-only mode, read what the user can read, and open any socket.

It buys two real things: the review's *input* is immutable (a node runs against a copy, and
capture already happened, so the snapshot under review cannot be altered by anything the node
does), and every mutation is captured. Note what that is not — an absolute-path write to the
checkout on disk is not prevented, and the case records that as open rather than calling it
covered. The environment a check runs with is not the sandbox's doing: the check runner
(`af_check::CheckRunner`) clears it and rebuilds it from an allowlist, with a `PATH` of
existing absolute directories only, and a container run starts from `--env-file /dev/null` plus
the declared variables. A host-local check ends with its process group, and on Linux with every
descendant that left the group (`setsid`) but kept the `AF_PROCESS_TREE` value it inherited,
before its tree is sealed; one that clears or replaces that variable, and on macOS any such
descendant, can outlive it. Each output stream of a check or a Worker is kept to 64 MiB: the rest is read
and discarded so the process never blocks, and a check or an Attempt that wrote more fails,
naming the cap.

A `ContainerProvider` also exists for hosts with a usable runtime. Finding `docker` on `PATH`
proves nothing: detection runs the runtime's own `info` and requires it to succeed. An
installed-but-unusable runtime reports `Isolation::None` and refuses to exec rather than falling
back to the host. The invocation it builds is asserted exactly — one bind, `--network=none`, no
inherited environment, `--pids-limit 4096`, `--cap-drop=ALL`, `--security-opt=no-new-privileges`
and `--init`. `--env-file /dev/null` keeps the host's environment out; it does not clear the
`ENV` the image itself declares, which is part of the pinned image. The image retains its own
pinned `PATH`; only host-independent check variables (`LC_ALL=C`, `TZ=UTC`, and declared
additions) are reintroduced. The shared provider retains explicit memory/CPU limits for callers
that supply them and its process ceiling for every container check. Worker development retains
its separately admitted memory, CPU, process and aggregate disk limits. Contained native checks
use the pinned `DEFAULT_IMAGE`, private runtime tmpfs directories and the same process supervision
and cleanup. Their wall is bounded by the existing check/Attempt deadline. The workload runs as
the caller's numeric UID:GID. Each execution has a unique runtime name: if the supervised client
fails or times out, bounded `rm -f` must confirm cleanup before sealing. Unconfirmed cleanup fails
the Attempt and preserves its sandbox rather than sealing or deleting a possibly live bind.

`make review-kernel-container-probes` retains the sandbox escape and timeout/reap probes and runs
`af-runtime::it task_review_execute_checks::a_container_check_executes_on_the_native_task_host`.
The native selector uses nextest's `--no-tests fail`; a retired exact name cannot silently pass.
These ignored live tests are explicitly executed when the e2e class is selected, including
`scripts/verify.sh --profile fast` and `make check`. A missing daemon is a hard failure.
The [containment record](security/containment-probes.md) distinguishes provider evidence from the
native host execution that must actually run.

Current Task Pipeline `/6` checks route through `CodeTaskDomain::checks`, including checks that
`ReviewTaskDomain` delegates to it. With `require_container = true`, the shared provider probes
runtime availability inside the Attempt deadline, materializes the Snapshot with its actual
isolation and passes ordinary `admit(Policy::safe(), ...)` before any check command. Unavailable
containment fails closed; a local directory cannot assert Container isolation. `CheckRunner`
still resolves typed arguments and the shared provider executes the resolved command. A contained
check receives private container HOME, TMPDIR, scratch and cache paths, never host runtime paths.
`[warm]` and native host-toolchain mappings remain incompatible with required containment.

`af/TaskCheckReceipt@2` binds exact plan, Snapshot, captured policy and named check results.
The additional `af/TaskCheckContainment@1` evidence, retained with the check Attempt, references
that receipt and records admitted Container isolation, the pinned image and observed execution
(`completed`, `refused` or `not_started`). Completion means captured process output, not a pass.
Failed commands and source mutation remain failed checks; the source is sealed against its
captured Manifest. Current check policy does not restore the retired Pipeline v2/v3 Gate binding
or `RunReport` representation. Worker execution retains its own admitted environment policy;
check containment never grants a Worker permission to run locally.

### Sealing

Sealing consumes the sandbox handle and rescans the tree, diffing it against the manifest it was
materialized from. What a node changed is therefore **derived, never reported** — which is what
makes the auto-apply rule checkable at all: a proposal must equal the kernel-computed diff, so an
unreverted debug probe fails it rather than riding along. Kind is part of that identity, so
swapping a file for a symlink to the same bytes counts as a change.

## The native Pipeline graph

Every Pipeline is the closed `af.pipeline/6` definition, with typed public inputs/outputs,
explicit node outputs, exact Worker bindings and captured policy. Compilation emits
`af.compiled-task/6` in `af/CompiledTask@4`. Native Review operations are ordinary primitive
operators; `review_domain`, `review_integration_checks` and the optional Campaign phase graph
are refused. The captured engine is `af.task-engine/4`; inspection is
`af/task-inspection@20`, recorded-plan inspection `af/task-plan-inspection@6`, and Experimental
execution plans `af.experiment-execution-plan/3` in `af/ExperimentExecutionPlan@3`.
These changed closed shapes have current writers, schemas and explicit retired-format refusal.
Pipeline `/6`, TaskKind `/3` and unchanged referenced subshapes keep their existing identities.

`af review plan --file FILE` captures one Task and records its proposed plan without an Attempt.
`af review run --file FILE` executes through the common scheduler. Its Authority Snapshot,
source candidate and optional Base are selected independently; a Diff must be nonempty before
checks, Provider admission or Workers. Continuation and repair use the same recorded native
Task lifecycle and allowances. See [Review Tasks](task-execution/review-task.md) for exact ports
and [the facade](task-execution/campaign-review.md) for commands and JSON contracts.

## One Review, end to end

A native Review binds its source/Base and prior history into an exact Subject and per-reviewer
assignments, runs required checks and independent Workers, and atomically reduces complete
results into canonical Finding and Demand Sets plus immutable history. Missing siblings yield
Incomplete, never an authoritative partial Ledger. Typed native Scatter records every owned
child and shard outcome and requires whole-Subject closeout and semantic closure.

Sealed proposals name exact original Reviewer Results, Report indices and their whole candidate
diff. Automatic Integration composes selected, disjoint candidates, refuses protected paths,
runs checks on the derived head and publishes a native commit with claims still pending full
successor review. Acceptance must actually select the exact committed head's complete successor
Review and its verdict; an earlier passing Round or static graph reachability cannot substitute.
Without a commit, ordinary admitted source transport retains its original provenance and does
not accidentally become authoritative Review evidence.

## Attempts and budgets

Common Task Attempts reserve tokens, Attempts and wall before dispatch. Failed, abandoned,
fenced and late work retains its exact charge; an overrun closes later admission rather than
forgetting spend. Retries use durable admitted feedback and retain their original bounds.
No Review-specific allowance, scheduler or Campaign-to-Task host remains. Native Review
conclusions bind exact cumulative accounting and a checked Task log prefix.

`TaskTransition@6` is the current closed transition family; `af/TaskRunReport@3` reports the
whole graph and carries no Campaign phase selector. `af/task-event@2` streams those current
records. Canonical Ledger, Report, Finding and `review.kernel/*` identities remain shared.
Read `af review inspect ID` for per-Attempt details and `af review report ID` for exact native
Task accounting. Generic storage registration, accounting, retention, leases and removal safety
are preserved; directory/category names containing Campaign do not authorize an old executor.

## Telemetry

Telemetry records what `af` does in a store of its own and is never read by the kernel
([ADR-0153](adr/0153-record-telemetry-in-its-own-store-and-never-let-it-steer.md);
[`telemetry.md`](telemetry.md)). Each phase is a `tracing` span emitted by the crate that owns
it; a span that does not name the run's identity (`af.store_id`, `af.run_id`, `af.run.kind`,
`af.execution_id`) takes it from its parent, and a phase that belongs to a run (a Store
operation, a git process, a capture, a check) is recorded only inside a span of one.
`af-process` links no telemetry (ADR-0026): the crates that receive its results record the
process spans from them.

### Telemetry span catalogue

Every span name af emits is a row here; `telemetry_span_catalogue` fails when a source names
one that is not. `af.turn` and `af.tool` are written by the harness stream tap
(`af_telemetry::stream`) at the arrival times of the lines that open and close them;
`af.sink` joins with the sink package of the design note.

| Span | Kind | Parent | Attributes |
| --- | --- | --- | --- |
| `af.command` | internal | root | `process.command`; at its end `process.exit_code` and the first run's `af.store_id`, `af.run_id` |
| `af.run` | internal | `af.command`; a measurement's: the measure Attempt | the run identity, `af.execution.ordinal`, `af.store.sequence_at_start`, `af.task_id`, `af.campaign`, `af.round`, `af.epoch`, `af.policy_rev`, `af.pipeline`, `af.pipeline.digest`; at its end `af.outcome` and the charge. A kernel measurement (ADR-0132) is its own run: `af.run.kind = measure`, its run id derived from the Task run, the Attempt and the measure, `af.measure`, and at its end `af.measurement.id` |
| `af.authority.load` | internal | `af.run` | `af.policy_rev`, `af.pipeline`, `af.pipeline.digest` |
| `af.plan` | internal | `af.run` | `af.plan_digest`, `af.pipeline`, `af.node_count`, `af.bindings`; one `node` record per plan node |
| `af.node` | internal | `af.run` | `af.node_id`, `af.node.kind`, `af.outcome` |
| `af.attempt` | internal | `af.node` | `af.attempt_id`, `af.epoch`, `af.worker`, `af.provider`, `af.provider.kind`, the kernel's usage and charge, outcome |
| `af.admission` | internal | `af.attempt` | `af.admission.reserved_tokens`, `af.provider`, `af.admission.wait_ms`, outcome (`refused` below the free-disk floor) |
| `af.context.compose` | internal | `af.attempt` | `af.context.manifest`, `af.context.bytes`, `af.context.estimated_tokens`, `af.context.layers` |
| `af.harness` | process | `af.attempt` | `gen_ai.operation.name = invoke_agent`, `gen_ai.provider.name`, `gen_ai.request.model`, `af.native`, the process attributes; for a tapped harness, at its end `dropped_chunks`, `gaps`, `af.stream.withheld`, `af.native.withheld` |
| `af.turn` | client | `af.harness` | `gen_ai.operation.name = chat`, `gen_ai.provider.name`, `gen_ai.request.model`, `af.series = turn`, `af.turn.ordinal`; at its end `af.corr`, the harness's per-turn `af.usage.*` and `gen_ai.usage.*`, `gen_ai.response.finish_reasons`, `gen_ai.response.model`, `af.turn.text_*`, `af.turn.thinking_*`, `af.turn.tool_calls`, outcome |
| `af.tool` | internal | `af.turn`, or `af.harness` outside a turn | `gen_ai.operation.name = execute_tool`, `gen_ai.tool.type = function`, `gen_ai.tool.name` (in the invocation's tool set), `gen_ai.tool.call.id` and `af.corr`, `af.tool.input_bytes`; at its end `af.tool.output_bytes`, `af.tool.exit_code`, `af.tool.status`, outcome |
| `af.output.parse` | internal | `af.attempt` | `af.output.contract`, `af.output.valid`, the refusal (`invalid_output_contract`) |
| `af.usage.account` | internal | `af.attempt` | `af.usage.observation`, `af.usage.unknown_cause`, `af.usage.chargeable_tokens` |
| `af.gate` | internal | a check node's `af.attempt`, or the review Gate's | `af.gate.checks`, `af.gate.decision`, `af.check.warm` |
| `af.check` | process | `af.gate`, or `af.measurement.repetition` | `af.check.name`, `af.check.fingerprint` (`af/check-fingerprint@1` or `unknown`), `af.check.status` (`passed`, `failed`, `not_run`), `af.check.warm`, the process attributes |
| `af.measurement.repetition` | internal | a measurement's `af.run` | `af.measurement.repetition`, outcome |
| `af.warm.restore`, `af.warm.save` | internal | a check node's `af.attempt` | `af.warm.key`, `af.warm.result` (`hit`, `miss`), `af.warm.kinds`, `af.warm.bytes` (available before, evicted after) |
| `af.snapshot.capture` | internal | any span of a run | `af.snapshot.kind` (`committed`, `worktree`, `sealed`), `af.snapshot`, `af.snapshot.files`, `af.snapshot.bytes` |
| `af.cas.materialize` | internal | any span of a run | `af.cas.op`, `af.cas.files`, `af.cas.bytes` |
| `af.store.append`, `af.store.read` | internal | any span of a run | `af.store.op` (`append`, `replay`), `af.store.events`, `af.store.bytes` |
| `af.git` | process | any span of a run | `af.git.op` (the subcommand), the process attributes; `ok` whenever git exited, its code in `process.exit_code` |
| `af.remote_check` | internal | `af.gate` | `af.remote_check.branch`, `af.remote_check.checks`, `af.remote_check.pull_request`, outcome |
| `af.process` | process | any span of a run | `af.process.kind` (`container command`, `container cleanup`, `runtime info probe`, `toolchain_probe`, `remote_check`), the process attributes |
| `af.sweep` | internal | `af.command` | `af.sweep.bytes_before`, `af.sweep.bytes_after`, `af.sweep.evicted` |

The process attributes of every `process` span: `process.pid`, `process.command` (the program
only), `process.exit_code`, `process.signal`, `af.rusage.user_ms`, `af.rusage.system_ms`,
`af.rusage.max_rss_bytes`, `af.stdout_bytes`, `af.stderr_bytes`, `af.stderr_held`. A figure the
platform did not deliver is absent, never zero.

## Native Task Review capabilities

Native Review uses `task/review.rs` over the common Task scheduler and Store. Its Integration and
Scatter modules own typed domain operations, sharing canonical Ledger reduction and the existing
owned-child registry and Attempt accounting. `TaskReviewIntegration@1` binds disjoint sealed
candidates to their original canonical claims; ordinary Task checks precede `TaskReviewCommit@1`,
whose claims await full discovery verification. `TaskReviewConclusion@1` binds final native Review
to one complete Task log prefix and exact cumulative charge. Native conclusion validation in `review_integration.rs` reads no Campaign Store or Campaign
prefix. The Campaign phase bridge, captured Review host and separate Campaign browser pane are
removed. The [capability record](design/current-contracts-and-task-review.md) identifies native
regressions, current Worker validation, completed consolidation and exact validation boundaries.

## Internal naming and protected external names

Workspace library packages and directories use responsibility names in kebab-case (`af-runtime`,
`af-pipeline-config`); Rust imports and modules use snake_case (`af_runtime`, `run_report`).
Importable Python modules also use snake_case, including compound names: `kernel_measure`,
`scenario_adapter`, `nightly_chain`, `evaluate_results` for offline recorded-result evaluation,
and `release_comparison` for durable release-comparison orchestration. Executable script paths
retain their public invocation spelling, including existing kebab-case entrypoints.

Internal navigation changes do not rename CLI commands or flags, `review.kernel/*` and `af/*`
artifact identities, persisted type strings, events, schema IDs or JSON shapes. Accepted ADRs
and raw captured historical evidence retain the names of the code they describe. Active examples
and navigation use current internal names. Worker instruction
references are active code navigation: a change to their bytes requires the corresponding package
version and native `af catalog digest` pin refresh, without changing Worker authority or budgets.

Native Review's Store contains Task transitions and typed native Round outputs. The shared
canonical Ledger is initialized from the Task Subject, with no Campaign manifest or bootstrap
event. `review.kernel/RunEvent@3` refuses retired event identities. Standalone continuation
validates each prior Round against its original captured plan and graph, then binds exact
canonical history and Base into a new Task plan under the existing history and Round bounds.
