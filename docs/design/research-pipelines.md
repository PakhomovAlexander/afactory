# Research Pipelines — design and implementation plan

**Status:** proposed, 2026-09-27. **Baseline:** kernel `main` 1447494 (after v0.9.0-rc.7; ADR-0118
and ADR-0120 give review and source-writing Workers a shell). **Execution model:** every package
below is one Task file kept outside the repository, compiled and run through the campaign
Pipeline `kernel/implementation-reviewed` from `.af/task-catalog.toml`. Package IDs are local
planning IDs, not issue or PR numbers.

## 1. Problem

The owner wants to run *research* through `af`: open-ended Tasks such as "experiment how the
release build can be optimized", "research and try how the `af` development cycle can take less
time and disk", and later report-only research that several models work on in different roles.
Today the runtime serves one shape of work — edit a Snapshot, gate it, verify it, deliver it — and
every research shape breaks one of its invariants:

- **Nothing measures.** A source-writing Worker with `execute-checks` can build and time a
  command in its sandbox (ADR-0120), but the number it reports is prose in an
  `af/ImplementationReport@1` summary. No operator runs a declared command under the kernel's
  control, records its wall time or the bytes it produced as a typed artifact, or compares two
  such artifacts. The only comparison machinery is the self-optimizer's `optimization_experiment`,
  whose cases, recipes and writable target are fixed to `.af/` configuration and harness files
  (`docs/task-execution/self-optimizer.md`).
- **Nothing accepts a report.** Seven installed profiles exist (`review_config::task::kind`), and
  a package cannot add one. The Document profile has no `source` port, so its author cannot read
  the repository; the Optimization profiles accept only the optimizer's own economics. An implement
  Task must end in a `SourceTree` a code policy verified.
- **Nothing chains.** ADR-0117 binds `source`, `history` and `sources` across Tasks and refuses
  every other port by name, so a measurement or a report recorded by one Task cannot feed the
  next. Research is iterative; a fixed DAG with one pre-execution Planner cannot iterate, and the
  campaign coordinator has to export artifacts to disk again.
- **The cycle itself is too expensive to research.** The TUI campaign's 38 Tasks charged about
  16.0M tokens and reached `satisfied` twice; each verification Task cost about 500k tokens and
  its check stage a median 15.6 minutes, because every Task check builds cold: `code.rs` gives
  each check a fresh `HOME`, `XDG_CACHE_HOME` and `CARGO_TARGET_DIR`
  (`crates/review-pipeline/src/task/code.rs`, `checks`), so `scripts/verify.sh`'s external target
  directory is never warm inside a Task. Thirty-five gates spent 8.6 hours compiling the same
  workspace. The Store under `$XDG_STATE_HOME/af` holds 22 GB with no `af task gc`, and about
  14 GB of stray `cargo` targets sit under `/private/tmp`.

## 2. Outcome and fixed requirements

After this campaign a research Task is an ordinary `af task`: a Pipeline measures a declared
command on a baseline and on a candidate the implementer produced, compares the two under a
declared objective, and an independent evaluator judges the result; or an author with read access
to the repository writes a report that an independent verifier on another principal accepts; and
the artifacts one Task records — measurements, comparisons, reports — bind into the next Task's
inputs by exact identity. The gates every package pays are warm.

These decisions are fixed for every package:

- **The kernel measures; a model never writes a number the kernel did not record.** A
  measurement is a declared command from the committed code policy, run by an installed operator
  in a read-only materialization of an exact Snapshot, repeated a declared number of times, with
  its wall time, exit status, output digest and any metrics the command itself reports in a
  typed line. A comparison is a deterministic fold over two such artifacts under a declared
  objective. Neither invokes a Provider. A model may propose the candidate and may read the
  comparison; it cannot author either artifact.
- **Warm caches are machine-local, bounded and never part of a Snapshot.** A Task check may reuse
  a build directory across Attempts and Tasks of one project only under the `trusted_local`
  isolation policy, only when the code policy declares it, keyed by the toolchain identity of the
  Snapshot, bounded in bytes, and removed rather than repaired when the key changes or the bound
  is exceeded. `require_container = true` refuses it, as ADR-0108 refuses Build Caches outside
  `trusted_local`. Cold or warm is recorded in `af/TaskRuntimeEvidence@1` so a measurement can
  say which it was.
- **A report is accepted by an independent verifier, not by its author.** The report profile
  reuses the Document renderer, checks and verification receipt; its author reads the source
  Snapshot and may run commands in an ephemeral clone that seals nothing back (ADR-0118's access
  for a non-writing Worker); its verifier runs on a distinct principal, receives the report, the
  requirements, the check receipt and the same source Snapshot, and its negative verdict stays
  negative. A report has no `snapshot` output and is never delivered to a worktree; `af task
  output --format markdown` is its only exit.
- **A bound input carries provenance only.** Extending ADR-0117 to every declared root port
  changes nothing about authority: the referenced Task must be recorded and finished, the artifact
  must verify in the CAS, type and cardinality must equal the port's exactly, and no acceptance,
  verification, plan approval, delivery or budget crosses the boundary.
- **The DAG stays fixed.** Iteration is a chain of Tasks whose inputs bind the previous Task's
  outputs. No loop operator and no mid-run re-planning are added; the Planner's one pre-execution
  run and its signed approval are unchanged.
- **Old records stay readable.** New Task-file fields, policy tables and receipt fields are
  optional and default to empty; an artifact whose payload shape changes gets a new version
  (ADR-0002); nothing that a GA release wrote is refused.
- **The self-optimizer keeps its slot.** `optimization_experiment`, its cases and recipes are
  not changed by this campaign. Its `latency` recipe is the natural later consumer of
  `af/Measurement@1`; that is recorded here as a follow-up, not a deliverable.
- **Nothing is weakened.** No contract, fixture, gate, budget or sandbox boundary changes to make
  a package pass. `trusted_local` is not security isolation, and no package pretends otherwise.

## 3. Execution model

Every package runs through `kernel/implementation-reviewed`: one implementer Attempt with a
shell (Claude Opus 5.5, high; ADR-0120), seal, `make check` and `markdownlint` as required checks,
two independent reviewers (`kernel/bugs`, GPT-5.6 Terra high; `kernel/correctness`, GPT-6 Sol
high) through `kernel/review-code`, review acceptance, one independent goal evaluation
(`kernel/evaluator`, GPT-6 Sol high), then explicit delivery to a new local worktree. The `uix`
reviewer of the TUI campaign is not required for these packages: none of them changes the
browser. A verification Task (`kernel/verification-reviewed`) re-runs the checks, both reviewers
and the evaluator on the delivered Snapshot. Each successor package captures the delivered,
locally committed predecessor.

The kernel that runs the campaign is built from this branch (`cargo build --release -p af`) and
invoked with `AF_DISPATCHED_FROM=1`, because the project lock pins a release without ADR-0120 and
without the operators the later packages add. R1 is delivered first so that every later
package's checks run warm.

Task files, the bindings file and the state directory live under
`$XDG_STATE_HOME/af/workstreams/research/`, outside every checkout. Provider labels in the
catalog are neutral (`claude-main`, `codex-main`); the uncommitted bindings file maps them to
machine-local accounts, and the independence policy requires distinct principals between the
implementer and every verifier.

```sh
W=$XDG_STATE_HOME/af/workstreams/research
AF=/private/tmp/af-research-target/release/af
AF_DISPATCHED_FROM=1 $AF task plan --file $W/tasks/r1.json --bindings $W/bindings.toml --state $W/state
AF_DISPATCHED_FROM=1 $AF task explain research-r1 --tree --state $W/state
AF_DISPATCHED_FROM=1 $AF task run research-r1 --confirm-plan <full PLAN id> --state $W/state
AF_DISPATCHED_FROM=1 $AF task deliver research-r1 --branch agent/research-r1 \
  --worktree ../afactory-wt-research-r1 --confirm research-r1 --state $W/state
```

A package that ends unverified is fixed by a human or an agent in its worktree and re-run as a
new Task revision; the failed Task's evidence and spend stay recorded. Reserves are not raised to
make a failed Attempt pass. Long runs are wrapped in `caffeinate -i -s`: an expired writer lease
after a sleep has already cost this project several verification Tasks.

## 4. Packages

### R1 — Warm Task checks

**Depends on:** nothing. Delivered first: every later package's Gate pays for it otherwise.

Deliverables:

1. `af.code-task-policy/1` gains an optional `[warm]` table: `build_cache = ["cargo_target"]`
   (the only kind this package installs), `caches = [...]` naming Cache Snapshot kinds resolved
   through the machine's cache policy exactly as a review Gate resolves `[gate] caches`
   (ADR-0036), and `max_bytes` (default 8 GiB, hard maximum 32 GiB). A policy that declares
   `[warm]` together with `require_container = true` is refused at load with a message naming
   both, mirroring ADR-0108's rule for Build Caches.
2. The check runner (`code.rs`, `checks`) binds `CARGO_TARGET_DIR` for a declared
   `cargo_target` to a persistent directory
   `$XDG_CACHE_HOME/af/task-build-cache/<project>/<toolchain>/cargo_target`, where `<project>`
   is the Store's repository identity and `<toolchain>` the digest of the Snapshot's
   `rust-toolchain.toml` or `rust-toolchain` bytes (absent file: the literal `none`). `HOME` and
   `XDG_CACHE_HOME` stay fresh per check. A declared Cache Snapshot binds `CARGO_HOME` and
   `CARGO_NET_OFFLINE` exactly as a Gate does, materialized into the runtime directory, never
   into the source tree.
3. Bounds and exclusion. One exclusive advisory lock per cache directory; a check that cannot
   take it within 60 seconds runs cold in its private runtime directory and records why. After a
   check, a directory above `max_bytes` is removed whole and recorded; a directory whose
   `<toolchain>` key no longer matches is never reused. The cache directory is created with mode
   `0700`, holds no credential, and is never read by candidate capture: it sits outside every
   sandbox.
4. Evidence. Every check records one `TaskCacheObservationV1` per declared kind — `kind`,
   `eligible`, `toolchain_id`, `bytes_available` before the check, `lookup_ms` and
   `materialization_ms` — beside the existing `Check` span; `bytes_available = 0` on an eligible
   observation is a cold check. `af task show` prints one line per check with elapsed time,
   cache kind and `warm <bytes>` or `cold <reason>`, and `af/task-inspection@11` carries the
   observations in its existing `runtime_observations` field.
5. This repository's `.af/code-policy.toml` declares `[warm] build_cache = ["cargo_target"]`;
   `scripts/verify.sh` honours a `CARGO_TARGET_DIR` that is already set instead of deriving one.
6. Tests and fixtures: a code Task run twice in one Store, whose second check observes
   `bytes_available > 0` and a shorter check span; a changed `rust-toolchain.toml` producing a
   cold check under a new key; a `max_bytes` of one byte producing removal after the check and a
   cold next check; a policy with `[warm]` and `require_container = true` refused at load; the
   busy-lock path running cold; and a byte-identity test that a warm check's derived Snapshot,
   candidate and delivery receipt equal the cold check's.
7. One ADR recording the decision and the options rejected (capturing the target into the CAS as
   ADR-0108 does; pointing every check at one shared host directory without a key; a
   per-Attempt clone of the previous check's output), linked from `docs/adr/README.md`; a
   `CHANGELOG.md` entry under Unreleased; a paragraph in `docs/tasks.md` on what `[warm]` grants
   and refuses.

Acceptance:

- On this repository, a second consecutive Task check of an unchanged workspace completes in
  under a third of the cold check's span, proven by the recorded `Check` spans of the fixture
  and stated for the real repository in the execution record.
- No warm check changes any Snapshot, candidate, receipt or `--json` document byte relative to a
  cold check, proven by the byte-identity test.
- `require_container = true` with `[warm]` is refused before any Worker or Provider admission.
- Existing fixtures and `--json` outputs without `[warm]` are byte-identical.

### R2 — Measure and compare

**Depends on:** R1 (a measurement declares whether it ran on a warm or a cold cache and needs
both to be possible).

Deliverables:

1. `af.code-task-policy/1` gains `[measures.<name>]` and `[objectives.<name>]`. A measure has a
   `command` (program plus args with `literal` or `untrusted` provenance, as a check has),
   `repetitions` (1 to 16), `warm` (`true` runs on the R1 cache, `false` runs cold in a fresh
   runtime directory), `wall_ms` per repetition (at most 3,600,000), and `metrics`: a list of
   `{ key, unit }` with `unit` one of `ms`, `bytes`, `count`, `ratio`; `elapsed_ms` is always
   recorded and needs no declaration. An objective names one `measure`, one `metric`, a
   `direction` (`lower` or `higher`), `min_improvement_ratio` (0 to 1) and `min_repetitions`
   (default 3).
2. A new artifact `af/Measurement@1`: the Snapshot ID, the measure name, the resolved command
   identity, the toolchain identity, `warm` and the observed cache bytes, one record per
   repetition — started time, `elapsed_ms`, exit status, stdout and stderr digests, the metrics
   parsed from the command's last stdout line when it is an `af.measure-report/1` JSON object
   (`{"schema":"af.measure-report/1","metrics":{"<key>":<number>}}`) — and per-metric
   `median`, `min`, `max` and `n`. A repetition that exits non-zero, times out or reports a
   metric with the wrong unit fails the measurement; a failed measurement is recorded with its
   receipts and its outcome is `failed`.
3. A new artifact `af/MeasurementComparison@1`: the two Measurement IDs, the objective, and per
   metric the baseline and candidate medians, the signed delta, the ratio, `n` on each side and a
   conclusion `improved`, `regressed`, `unchanged` or `inconclusive`. The comparison's outcome is
   `passed` when the objective's metric improved by at least `min_improvement_ratio` with at
   least `min_repetitions` on both sides, `failed` when it regressed or the improvement is below
   the threshold with sufficient repetitions, and `inconclusive` otherwise or when either
   measurement failed. Medians are exact decimals; no metric is ever saturated or clamped.
4. Two installed operators in the code domain, closed like every other member of
   `TaskOperatorV1`: `measure { measures }` (input `source: af/SourceTree@1`, one
   `af/Measurement@1` output per declared measure, `outcome_port` semantics so `when` and
   `select` can branch on it) and `compare { objective }` (inputs `baseline` and `candidate`,
   each `af/Measurement@1` with `cardinality = "one"`, output `af/MeasurementComparison@1`
   with an outcome). Both materialize nothing writable, invoke no Provider, and run under the
   Task's check wall allowance; the plan compiler refuses a `compare` whose two inputs name
   different measures or a `measure` whose name the captured policy lacks.
5. A `kernel/experiment` Pipeline package (this repository) and a `builtin/experiment` starter:
   `measure` the source as the baseline; implementer (`write-source`, `execute-checks`) → seal
   → checks → `measure` the candidate → `compare` → independent evaluator (`verify`) whose
   contract declares an additional `comparison` input beside `checks`, `requirements` and
   `source` → `accept`. The Task kind is `implement` with `verification = "evaluation"`; public
   outputs are `snapshot`, `verification`, `baseline`, `candidate` and `comparison`. The evaluator
   is asked whether the requirements are met given the comparison; a `failed` comparison cannot
   be talked into a `passed` evaluation, because the Pipeline gates the evaluator on the
   comparison outcome with `when`.
6. `af task output --port comparison --format markdown` renders a comparison as one table;
   `--format json` is unchanged. `af task show` prints each measurement's median elapsed time and
   each comparison's conclusion.
7. Tests and fixtures: a credential-free fixture repository whose measured command is a Python
   script that writes a deterministic number of bytes and reports `bytes_written`, with an
   improving candidate (comparison `passed`), a regressing candidate (`failed`), `repetitions = 1`
   under `min_repetitions = 3` (`inconclusive`), a non-zero exit (measurement `failed`,
   comparison `inconclusive`, evaluator never dispatched), and a malformed report line; schema
   parity entries for both artifacts and the two policy tables.
8. One ADR (options rejected: letting the implementer report its own numbers; a generic
   `command` operator with untyped output; reusing `optimization_experiment` for source
   candidates), a `CHANGELOG.md` entry, and `docs/task-execution/experiments.md` linked from
   `docs/README.md` and `docs/task-execution.md`.

Acceptance:

- `af catalog test` passes for `kernel/experiment` and the starter; `af task plan` compiles the
  experiment Pipeline on this repository with zero Attempts.
- The fixture's five outcomes are pinned by tests, and a comparison over the same two
  Measurements replays byte-identically without spending an Attempt.
- No measurement or comparison payload carries a value the kernel did not observe or compute.
- Existing Pipelines, fixtures and `--json` outputs are byte-identical.

### R3 — Report Tasks

**Depends on:** nothing in code; sequenced after R2 so the campaign's experiment can feed its
first report.

Deliverables:

1. A new installed profile `TaskKindProfile::Report`, built-in kind string `report`, also
   reachable through an `af.task-kind/1` package. Inputs: `requirements` (`af/Requirements@1`),
   `source` (`af/SourceTree@1`), optional `sources` (`af/DocumentSources@1`). Required outputs:
   `report: af/Document@1` and `verification: af/DocumentVerification@1` covering `verified`.
   Allowed effects: `read-source` and `execute-checks`; `write-source` is refused. The profile has
   no `snapshot` output, so `af task deliver` refuses a report Task before any Git mutation with
   a message naming `af task output`.
2. `worker_access` grants `ExecuteChecks` — an ephemeral-write clone that seals nothing back and
   a shell — to a Worker whose `roles` contain `author` and whose effects are `read-source`
   plus `execute-checks` without `write-source`, exactly as ADR-0118 grants it to the `review`
   role. The effects table in `docs/task-execution.md` gains the row.
3. `af/DocumentDraft@2`: `af/DocumentDraft@1` plus an optional `path` and `line` on a citation,
   naming an entry of the source Snapshot's Manifest. The renderer prints such a citation as
   `path:line`; `document_check` verifies that every cited path exists in the Manifest and every
   cited source key exists in `sources`. The report profile accepts drafts of either version;
   the Document profile is unchanged.
4. `af.document-task-policy/1` gains optional `max_source_entries` (default 32, maximum 256)
   and `max_source_bytes` (default 256 KiB, maximum 4 MiB), and the report profile reads them.
5. The Pipeline shape is the Document Pipeline with a source: author (`verify`-style Worker slot
   with `roles = ["author"]`) → `document_seal` → `document_check` → verifier (independent of
   the author; its contract declares `requirements`, `document`, `checks` and `source`) →
   `document_accept`. A `kernel/report` Pipeline package with `kernel/analyst` (Claude Opus 5.5,
   high, `read-source` + `execute-checks`, 1.5M tokens, 3 hours) and `kernel/report-verifier`
   (GPT-6 Sol, high, `read-source`) for this repository; a `builtin/report` starter with
   command substitutes for `af catalog init --profile report`.
6. `af task output --port report --format markdown` works unchanged; `af task show` prints the
   report's title and the verifier's outcome.
7. Tests and fixtures: the starter's command author writes a report citing two paths, the
   verifier accepts it; a citation of an absent path fails `document_check`; a negative verifier
   verdict stays `unsatisfied`; a report Task with `write-source` on its author is refused at
   planning; delivery of a satisfied report Task is refused with the named message; schema
   parity for `DocumentDraft@2` and the policy fields.
8. One ADR (options rejected: widening the Document profile in place; an implement Pipeline with
   a report side output; a free-form `research` profile with model-judged acceptance), a
   `CHANGELOG.md` entry, `docs/task-execution/report.md` linked from `docs/README.md`, and
   CONTEXT.md terms **Measurement**, **Comparison** and **Report Task** with the nearby terms they
   must not be confused with (Evidence, Demand, Document).

Acceptance:

- `af catalog init --profile report` produces a directory whose Task runs to `verified` with
  three command Attempts and no credential, and `af task output` writes its Markdown.
- The author's sandbox seals byte-identical to its source; an author that edits its source fails
  its Attempt with the ADR-0118 message.
- A report Task cannot be delivered; a Document Task and an implement Task are unchanged,
  proven by byte-identical existing fixtures.

### R4 — Bind any declared root port

**Depends on:** R2 and R3 (the ports worth binding exist).

Deliverables:

1. ADR-0117's binding rule is widened: a Task file's `inputs` table may bind any root input port
   the selected Pipeline declares, when the referenced Task is recorded and `Finished`, the
   named output is in its result, every artifact verifies in the CAS, and the recorded type and
   cardinality equal the port's exactly. `requirements`, `base` and `continuation` stay refused
   by name for the reasons ADR-0117 gives. `source` keeps its re-rooting rules unchanged.
2. `af/TaskInputBindings@1` records every binding as today; `af task explain` and `af task
   show` display them unchanged.
3. A three-Task fixture in one Store: the R2 experiment Task, a report Task binding
   `comparison` and `source` to it, and a second report Task binding `sources` to a Document
   Task's `document` output being refused because the types differ.
4. One ADR amending ADR-0117 (status note on 0117's line), a `CHANGELOG.md` entry, and the
   updated `docs/task-execution/task-inputs.md`.

Acceptance:

- A bound `comparison` reaches the report author as an exact artifact, proven by the fixture's
  context manifest.
- Every refusal is raised before any Worker or Provider admission and names the port and the
  type mismatch.
- A Task file without `inputs` keeps byte-identical revision, plan and inspection documents.

### R5 — Store hygiene: `af task gc` and sizes

**Depends on:** nothing; sequenced last because it changes no research behaviour.

Deliverables:

1. `af task list --sizes` prints, per Task, the bytes of CAS objects only that Task references
   and the bytes it shares with other Tasks, plus the Store total and object count; `--json`
   carries the same numbers.
2. `af task gc --state DIR --older-than DAYS --keep N [--apply]` previews and, with `--apply`,
   collects finished Tasks beyond the newest `N` whose last event is older than `DAYS`: it
   appends one `task_collected` event naming the Task, the time and the bytes freed, then removes
   every CAS object no retained Task references. A Task that is `Running`, holds a writer lease
   or is referenced by another Task's `af/TaskInputBindings@1` is never collected, and the
   preview says so. Without `--apply` nothing is written.
3. A collected Task's projection reports `collected <time>` with its ID, kind, outcome and
   chargeable tokens; `af task show` prints that instead of the artifact-backed sections, and
   `af task output` refuses with the same word. Replay of a collected Task never treats a missing
   artifact as corruption.
4. Tests and fixtures: two finished Tasks, `--keep 1` collecting the older one, its exclusive
   objects gone, the shared objects and the retained Task's `show` byte-identical; a running Task
   refused; a Task referenced by a binding refused; `gc` without `--apply` writing nothing.
5. One ADR (options rejected: deleting a Task's events; a reference count kept in the CAS;
   `af review gc` semantics of removing whole directories), a `CHANGELOG.md` entry, and a
   paragraph in `docs/tasks.md`.

Acceptance:

- After `gc --apply`, every retained Task's `show --json` document is byte-identical to before.
- The Store's byte total drops by exactly the previewed amount on the fixture.
- No command other than `gc --apply` ever removes a CAS object.

### R6 — First research Tasks (campaign closure, no package code)

With R1–R4 delivered and the kernel built from the branch, two research Tasks run against this
repository and their spend and outcome are recorded in §6:

1. **Release build.** An experiment Task whose measure is `cargo build --release -p af --locked`
   with `warm = false`, `repetitions = 3`, metrics `elapsed_ms` and `target_bytes` (reported by a
   wrapper script), objective `elapsed_ms lower by 0.10`. The implementer may change `Cargo.toml`
   profiles, feature flags and the release script and nothing else, stated in the requirements.
2. **Cycle time and disk.** A report Task whose author reads this repository, the recorded
   `af/TaskRuntimeEvidence@1` of the campaign's own Tasks (bound through R4) and the experiment's
   comparison, and answers where the cycle's time and disk go and which change this plan should
   make next.

If either Task fails, the failure and its evidence are the result; no number is invented.

## 5. Validation and rollback discipline

Each package is one Task with its own reviewers and evaluator, delivered to its own worktree and
verified there by a verification Task before the next package captures it. A package that fails
review or verification is fixed in its worktree by a human or an agent and re-run as a new Task
revision. Anything unverified is not merged. The design itself is reviewed once, by one GPT-6 Sol
(high) reviewer over the `design-review` Pipeline, before R1 is planned; its Findings are fixed in
this document and recorded in §6.

## 6. Execution record

Filled as the campaign runs: Task IDs, plan identities, chargeable tokens, Findings and their
dispositions, kernel defects surfaced, and the measured cold and warm check spans on this
repository.
