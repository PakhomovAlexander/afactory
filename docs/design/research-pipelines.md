# Research Pipelines — design and implementation plan

**Status:** proposed 2026-09-27; reviewed the same day (§6) and revised. **Baseline:** kernel `main` 1447494 (after v0.9.0-rc.7; ADR-0118
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
  with a read-only materialization of an exact Snapshot as its working directory and a private,
  bounded, writable runtime directory for everything the command produces, repeated a declared
  number of times, with its wall time, exit status, output digests and any metrics the command
  itself reports in a typed line that names each value's unit. The source is verified unchanged
  after every repetition and the runtime directory is discarded once its metrics are recorded. A
  comparison is a deterministic fold over two such artifacts under a declared objective, with
  exact decimal arithmetic and stated rules for even samples, a zero baseline and an unchanged
  result. Neither invokes a Provider. A model may propose the candidate and may read the
  comparison; it cannot author either artifact.
- **A Warm Check Cache is an explicitly unsafe, machine-local layer that only a check ever
  touches.** A Task check may reuse a build directory across Attempts and Tasks of one project
  only under the `trusted_local` isolation policy, only when the committed code policy declares
  it, keyed by the resolved toolchain identity (declaration bytes, `rustc -vV`, `cargo -vV`,
  target triple and the check's fixed environment), bounded in bytes during and after the check,
  and removed rather than repaired when the key changes, the bound is exceeded or the directory
  is otherwise suspect. It is never cloned into a Worker sandbox, never enters a Snapshot,
  candidate or delivered tree, and `require_container = true` refuses it. This deliberately
  widens ADR-0108's one-Round Build Cache scope for checks alone; the R1 ADR says so, supersedes
  that clause in part, and states the admission, validation and invalidation rules. Cold or warm
  is recorded in `af/TaskRuntimeEvidence@1`, so a measurement can say which it was.
- **A report is accepted by an independent verifier, not by its author, and its acceptance is
  bound to the exact source Snapshot it cites.** The report profile reuses the Document renderer
  and the Document Task's shape, with its own versioned sources, check and verification
  artifacts that retain the source Snapshot identity; its author reads the source Snapshot and
  may run commands in an ephemeral clone that seals nothing back (ADR-0118's access for a
  non-writing Worker); its verifier runs on a distinct principal, receives the report, the
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

1. `af.code-task-policy/1` gains an optional `[warm]` table: `build_cache` naming warm kinds —
   `cargo_target` (the build directory, bound as `CARGO_TARGET_DIR`) and `cargo_home` (Cargo's
   registry and git caches, bound as `CARGO_HOME`; the kernel never places a credential in it
   and treats one that holds `credentials.toml` as suspect) — `caches = [...]` naming Cache
   Snapshot kinds resolved through the machine's cache policy exactly as a review Gate resolves
   `[gate] caches` (ADR-0036), and `max_bytes` (default 8 GiB, hard maximum 32 GiB, shared by
   every warm kind of one toolchain key). A policy that declares `[warm]` together with
   `require_container = true` is refused at load with a message naming both, mirroring
   ADR-0108's rule for Build Caches. A declared `caches = ["cargo"]` Cache Snapshot takes
   precedence over a `cargo_home` warm directory for `CARGO_HOME`: the two are never bound at
   once, and the superseded kind records an ineligible `cargo_home:superseded` observation.
2. The check runner (`code.rs`, `checks`) binds each declared warm kind to a persistent
   directory `$XDG_CACHE_HOME/af/task-build-cache/<project>/<toolchain>/<kind>`, where
   `<project>` is the Store's repository identity and `<toolchain>` is the digest of: the
   Snapshot's `rust-toolchain.toml` or `rust-toolchain` bytes (absent: the literal `none`), the
   complete `rustc -vV` and `cargo -vV` output of the `rustc` and `cargo` the check's `PATH`
   resolves, the host target triple, and the check's fixed environment (`PATH`, `LC_ALL`, `TZ`,
   `RUSTUP_HOME`). The two version commands run once per check Attempt before the first check,
   under the check's own environment and a 30-second bound; a failure to resolve either means no
   warm cache and a recorded reason. `HOME` and `XDG_CACHE_HOME` stay fresh per check. A
   declared Cache Snapshot binds `CARGO_HOME` and `CARGO_NET_OFFLINE` exactly as a Gate does,
   materialized into the runtime directory, never into the source tree.
   Under `[warm]` the check environment also carries the kernel's rustup home — `RUSTUP_HOME`
   from the kernel's own environment, else `$HOME/.rustup` of the kernel's `HOME` when that
   directory exists — and `RUSTUP_AUTO_INSTALL=0`, for the probe and for the check alike. Without
   it a rustup proxy `rustc` or `cargo` sees the check's empty `HOME`, downloads the whole
   toolchain into it before answering, and does so again on every check; with it the installed
   toolchain answers at once, and a toolchain the machine lacks is a probe failure
   (`cold toolchain_unresolved`), never a download. The rustup home is read by the toolchain,
   not written by the check: it is not a warm kind, carries no bound and is never removed.
3. Bounds and exclusion. One exclusive advisory lock per cache directory; a check that cannot
   take it within 60 seconds runs cold in its private runtime directory and records why.
   `max_bytes` is enforced three times: before the check (a directory already above the bound
   is removed and the check runs cold), during the check (a monitor samples the directory's
   size at most every five seconds and, above the bound, ends the check's process group through
   the supervised kill path, records the check as `failed` with reason
   `warm_cache_bound_exceeded`, and removes the directory before the lock is released), and
   after the check (a directory above the bound is removed and recorded, so the next check runs
   cold). A directory whose `<toolchain>` key no longer matches is never reused. The cache
   directory is created with mode `0700`, holds no credential, is never read by candidate
   capture and is never mounted, cloned or copied into any Worker sandbox: it sits outside every
   sandbox, and only the kernel's check runner opens it.
4. Evidence. Every check records one `TaskCacheObservationV1` per declared kind — `kind`,
   `eligible`, `toolchain_id`, `bytes_available` before the check, `lookup_ms` and
   `materialization_ms` — beside the existing `Check` span; `bytes_available = 0` on an eligible
   observation is a cold check, and an ineligible observation names its reason in `kind`
   (`cargo_target:busy`, `cargo_target:toolchain_unresolved`, `cargo_target:bound_exceeded`).
   `af task show` prints one line per check with elapsed time, cache kind and `warm <bytes>` or
   `cold <reason>`, and `af/task-inspection@11` carries the observations in its existing
   `runtime_observations` field.
5. This repository's `.af/code-policy.toml` declares
   `[warm] build_cache = ["cargo_target", "cargo_home"]`; `scripts/verify.sh` honours a
   `CARGO_TARGET_DIR` that is already set instead of deriving one.
6. Tests and fixtures, all deterministic: a code Task run twice in one Store, whose second
   check observes `bytes_available > 0` under the same `toolchain_id` and whose derived
   Snapshot, candidate, delivery receipt and check outcome equal the first run's; a changed
   `rust-toolchain.toml` producing a cold check under a new key; a stubbed `rustc` on `PATH`
   reporting a different version producing a new key; a `max_bytes` of one byte producing the
   pre-check removal and a cold check; a check whose command writes past the bound being ended
   with `warm_cache_bound_exceeded` and the directory gone afterwards; a policy with `[warm]`
   and `require_container = true` refused at load; the busy-lock path running cold; and a Task
   without `[warm]` producing byte-identical revision, plan, inspection and delivery documents
   to today's.
7. One ADR recording the decision and the options rejected (capturing the target into the CAS
   as ADR-0108 does; pointing every check at one shared host directory without a key; a
   per-Attempt clone of the previous check's output), superseding ADR-0108's one-Round scope in
   part for Task checks only and stating the admission, validation and invalidation rules of §2,
   linked from `docs/adr/README.md`; a `CHANGELOG.md` entry under Unreleased; a paragraph in
   `docs/tasks.md` on what `[warm]` grants and refuses.

Acceptance:

- Cache selection, reuse and integrity are pinned by the deterministic fixtures above. The
  speed claim is benchmark Evidence, not a fixture gate: §6 records at least five paired cold
  and warm checks of this repository on the same pinned Snapshot and toolchain, with each
  `Check` span, the median ratio, the spread and the machine's load and power conditions.
- A warm check changes no Snapshot, candidate, delivery tree or check outcome relative to a cold
  check; only `runtime_observations` and the `af task show` cache line differ, and they differ
  accurately. A Task whose policy has no `[warm]` is byte-identical to today in every document.
- `require_container = true` with `[warm]` is refused before any Worker or Provider admission.
- No Worker sandbox, Snapshot or delivered tree ever contains a byte of the cache directory,
  proven by the fixture's manifests.

### R2 — Measure and compare

**Depends on:** R1 (a measurement declares whether it ran on a warm or a cold cache and needs
both to be possible).

Deliverables:

1. `af.code-task-policy/1` gains `[measures.<name>]` and `[objectives.<name>]`. A measure has a
   `command` (program plus args with `literal` or `untrusted` provenance, as a check has),
   `repetitions` (1 to 16), `warm` (`true` binds the R1 cache as `CARGO_TARGET_DIR`; `false`
   binds a fresh directory inside the repetition's runtime directory), `wall_ms` per repetition
   (at most 3,600,000), and `metrics`: a list of `{ key, unit }` with `unit` one of `ms`,
   `bytes`, `count`, `ratio`; `elapsed_ms` is the built-in metric, always recorded, and a
   declared metric may not reuse its key. An objective names one `measure`, one `metric`, a
   `direction` (`lower` or `higher`), `min_improvement_ratio` (0 to 1 inclusive, written as
   canonical decimal text such as `"0.1"` or as the integer 0 or 1; a TOML float is refused
   because the parser rounds it before the kernel can capture it) and `min_repetitions` (1 to
   16, default 3). The plan compiler refuses a plan whose
   `measure` nodes' summed `repetitions × wall_ms` exceeds the captured `check_wall_ms` (a
   measure Attempt owns no checks), and it refuses a `measure` whose name the captured policy
   lacks or a `compare` whose two inputs name different measures.
2. Execution. Each repetition runs with the read-only source materialization as its working
   directory and a private runtime directory holding `HOME`, `TMPDIR`, `XDG_CACHE_HOME` and,
   for `warm = false`, `CARGO_TARGET_DIR`; a declared Cache Snapshot binds `CARGO_HOME` as in
   R1. After every repetition the source Manifest is re-verified and a changed or added source
   entry fails the measurement with the same message a mutated check produces. The runtime
   directory is measured by the command itself (a wrapper reports the bytes it cares about),
   then discarded. A repetition that exits non-zero, exceeds its `wall_ms`, is cut by the
   Attempt deadline, or reports a malformed line fails the measurement: no later repetition
   runs, the failed repetition's receipts are retained, and the measurement's outcome is
   `failed` with the reason (`exit` — which is also what a repetition the warm cache's bound ends
   records — `timeout`, `deadline`, `malformed_report`, `unit_mismatch`, `source_mutated`). A
   measurement is never silently partial.
3. A new artifact `af/Measurement@1`: the Snapshot ID, the measure name, the resolved command
   identity, the R1 `toolchain_id` and `warm`, one record per repetition — started time,
   `elapsed_ms`, exit status, stdout and stderr digests, the cargo target it actually ran
   against when the measure asked for the warm cache (warm or cold, its bytes, and the reason
   when it ran cold), the metrics parsed from the command's last stdout line when it is an `af.measure-report/1` JSON object
   `{"schema":"af.measure-report/1","metrics":{"<key>":{"value":"<decimal>","unit":"<unit>"}}}`,
   whose keys must equal the declared set and whose units must equal the declared units — and
   per metric `median`, `min`, `max` and `n`. Values are finite, non-negative decimals with at
   most 38 significant digits, encoded as canonical decimal strings, the encoding
   `TaskTokenUsageV3` already uses for charges. The median of an even sample is the exact mean
   of the two middle values.
4. A new artifact `af/MeasurementComparison@1`: the two Measurement IDs, the objective, and per
   metric the baseline and candidate medians, the signed improvement (`baseline − candidate`
   for `lower`, `candidate − baseline` for `higher`), the ratio (`improvement / baseline`, kept as a fraction in lowest terms
   because it rarely terminates as a decimal, and absent with a zero baseline) and `n` on each
   side, with a conclusion: `improved` when the improvement is strictly positive and the ratio
   is at least `min_improvement_ratio`; `below_threshold` when it is strictly positive and the
   ratio is not, so an insufficient improvement is visible as such; `regressed` when the
   improvement is strictly negative; `unchanged` when the improvement is zero, including a zero baseline with a
   zero candidate; `inconclusive` when either side has fewer than `min_repetitions`, either
   measurement failed, or the baseline is zero and the candidate is not. The comparison's
   outcome is `passed` only for `improved` on the objective's metric, `failed` for `regressed`,
   `unchanged` or `below_threshold`, and `inconclusive` otherwise. `min_improvement_ratio = 0` therefore still requires a strictly
   positive improvement. No value is saturated, clamped or rounded.
5. Two installed operators in the code domain, closed like every other member of
   `TaskOperatorV1`: `measure { measures }` (input `source: af/SourceTree@1`, one
   `af/Measurement@1` output per declared measure, `outcome_port` semantics so `when` and
   `select` can branch on it) and `compare { objective }` (inputs `baseline` and `candidate`,
   each `af/Measurement@1` with `cardinality = "one"`, output `af/MeasurementComparison@1`
   with an outcome). Both invoke no Provider and run inside the Task's check wall allowance
   under the compiler rule in deliverable 1.
6. A `kernel/experiment` Pipeline package (this repository) and a `builtin/experiment` starter:
   `measure` the source as the baseline; implementer (`write-source`, `execute-checks`) → seal
   → checks → `measure` the candidate → `compare` → independent evaluator (`verify`) whose
   contract declares an additional `comparison` input beside `checks`, `requirements` and
   `source` → `accept`. Because `when` takes one receipt, the candidate measure, the comparison
   and the evaluator sit in a child Pipeline called only after the checks pass, and the
   evaluator inside it is gated with `when` on the comparison passing: a `failed` or
   `inconclusive` comparison never reaches an evaluator, and the Task ends `incomplete` with the
   comparison as its public explanation. The Task kind is `implement` with
   `verification = "evaluation"`; public outputs are `snapshot`, `verification`, `baseline` and,
   produced only after passed checks, `candidate` and `comparison`. The Pipeline runs one node
   at a time, so no measurement shares the machine with other work of the same Task.
7. This repository's `.af/code-policy.toml` declares `[measures.release_build]`: command
   `scripts/measure-release.sh`, `repetitions = 3`, `warm = false`, metrics `target_bytes` and
   `binary_bytes` (both `bytes`); and `[objectives.release_build_time]`: measure
   `release_build`, metric `elapsed_ms`, direction `lower`, `min_improvement_ratio = 0.10`,
   `min_repetitions = 3`. The committed `scripts/measure-release.sh` runs
   `cargo build --release -p af --locked` with the `CARGO_TARGET_DIR` the kernel bound, then
   prints the report line from that directory's byte total and the `af` binary's size. Because a
   Task Worker may not edit `.af/`, the implementer stages the tables and the `kernel/experiment`
   packages under `fixtures/kernel-experiment/` with their install steps, a test performs those
   steps on a copy of `.af/`, and a human installs them into the repository.
8. `af task output --port comparison --format markdown` renders a comparison as one table;
   `--format json` is unchanged. `af task show` prints each measurement's median elapsed time and
   each comparison's conclusion.
9. Tests and fixtures: a credential-free fixture repository whose measured command is a Python
   script that writes a deterministic number of bytes into `$TMPDIR` and reports `bytes_written`
   with its unit, with an improving candidate (comparison `passed`), a regressing candidate
   (`failed`), an unchanged candidate (`failed`), `repetitions = 1` under `min_repetitions = 3`
   (`inconclusive`), a non-zero exit (measurement `failed`, comparison `inconclusive`,
   evaluator never dispatched), a report line with `unit = "count"` for a `bytes` metric
   (`unit_mismatch`), a command that writes into the source (`source_mutated`), an even sample
   whose median is the exact mean, a zero baseline, and a plan whose repetition budget exceeds
   `check_wall_ms` refused at compile; schema parity entries for both artifacts and the two
   policy tables.
10. One ADR (options rejected: letting the implementer report its own numbers; a generic
    `command` operator with untyped output; reusing `optimization_experiment` for source
    candidates), a `CHANGELOG.md` entry, and `docs/task-execution/experiments.md` linked from
    `docs/README.md` and `docs/task-execution.md`.

Acceptance:

- `af catalog test` passes for `kernel/experiment` and the starter; `af task plan` compiles the
  experiment Pipeline on this repository, with the committed `release_build` measure and
  objective, with zero Attempts.
- The fixture's outcomes above are pinned by tests, and a comparison over the same two
  Measurements replays byte-identically without spending an Attempt.
- No measurement or comparison payload carries a value the kernel did not observe or compute,
  and the source Snapshot is unchanged after every repetition.
- Existing Pipelines, fixtures and `--json` outputs are byte-identical.

### R3 — Report Tasks

**Depends on:** R2 (the report profile declares the `comparison` and `measurements` ports whose
types R2 installs).

Deliverables:

1. A new installed profile `TaskKindProfile::Report`, built-in kind string `report`, also
   reachable through an `af.task-kind/1` package. Root inputs: `requirements`
   (`af/Requirements@1`), `source` (`af/SourceTree@1`), optional `sources`
   (`af/ReportSources@1`), optional `comparison` (`af/MeasurementComparison@1`,
   `cardinality = "one"`) and optional `measurements` (`af/Measurement@1`,
   `cardinality = "many"`). Required outputs: `report: af/Document@1` and
   `verification: af/ReportVerification@1` covering `verified`. Allowed effects: `read-source`
   and `execute-checks`; `write-source` is refused. The profile has no `snapshot` output, so
   `af task deliver` refuses a report Task before any Git mutation with a message naming
   `af task output`.
2. `af/ReportSources@1`: zero to 256 captured entries of `{ title, uri, revision, text }`, each
   at most 256 KiB and at most 512 KiB in total (a report's context must fit the 1 MiB Worker
   request beside its other inputs), in a file of at most 640 KiB, declared in a Task file as
   `report_sources =
   "<path>"` in the `af.document-sources/1` file shape the Document profile already reads. The
   Document profile and `af/DocumentSources@1` are unchanged.
3. `worker_access` grants `ExecuteChecks` — an ephemeral-write clone that seals nothing back and
   a shell — to a Worker whose `roles` contain `author` and whose effects are `read-source`
   plus `execute-checks` without `write-source`, exactly as ADR-0118 grants it to the `review`
   role. The effects table in `docs/task-execution.md` gains the row.
4. `af/DocumentDraft@2`: `af/DocumentDraft@1` plus an optional repository citation of
   `{ path, line? }`, where `path` is spelled exactly as `review_core::encode_path` spells the
   Manifest entry it names, that entry is a regular or executable file (not a symlink or a
   directory) whose first 8 KiB hold no NUL byte, and `line`, when present, is at least 1 and
   at most the file's line count. The renderer prints such a citation as `path` or `path:line`.
   The report profile accepts drafts of either version; the Document profile is unchanged.
5. Three installed operators for the report profile, mirroring the Document ones:
   `report_seal` (renders the draft to `af/Document@1` and records the source Snapshot and
   Manifest IDs), `report_check` (the Document checks plus every repository citation resolved
   against that exact Manifest, producing `af/ReportCheckReceipt@1`, which retains the
   document, sources, policy and source Snapshot identities) and `report_accept` (producing
   `af/ReportVerification@1`, which retains the acceptance invocation, exact Document, policy,
   check receipt, selected evaluation and source Snapshot). Sealing, checks, verifier admission
   and acceptance all revalidate the same Snapshot ID through `same_as` port affinity; a
   verifier whose `source` differs from the check receipt's is refused at admission.
6. The Pipeline shape is the Document Pipeline with a source: author (`roles = ["author"]`)
   → `report_seal` → `report_check` → verifier (independent of the author; its contract
   declares `requirements`, `document`, `checks`, `source` and the optional `comparison` and
   `measurements`) → `report_accept`. A `kernel/report` Pipeline package with `kernel/analyst`
   (Claude Opus 5.5, high, `read-source` + `execute-checks`, 1.5M tokens, 3 hours) and
   `kernel/report-verifier` (GPT-6 Sol, high, `read-source`) for this repository; a
   `builtin/report` starter with command substitutes for `af catalog init --profile report`.
   The author and the verifier receive `comparison` and `measurements` as exact artifacts in
   their context manifests when bound, and nothing when absent.
7. `af task output --port report --format markdown` works unchanged; `af task show` prints the
   report's title, the verifier's outcome and the cited Snapshot.
8. Tests and fixtures: the starter's command author writes a report citing two paths and one
   `path:line`, the verifier accepts it; a citation of an absent path, of a directory, of a
   symlink, of a file with a NUL byte, or of a line past the end fails `report_check`; a report
   Task without `report_sources` runs with an empty set; a sources file over its bound is refused at
   capture; a negative verifier verdict stays `unsatisfied`; a verifier bound to a different
   Snapshot is refused at admission; a report Task with `write-source` on its author is refused
   at planning; delivery of a satisfied report Task is refused with the named message; schema
   parity for the three new artifacts, `DocumentDraft@2` and `ReportSources@1`.
9. One ADR (options rejected: widening the Document profile in place; an implement Pipeline with
   a report side output; a free-form `research` profile with model-judged acceptance), a
   `CHANGELOG.md` entry, `docs/task-execution/report.md` linked from `docs/README.md`, and
   CONTEXT.md terms **Measurement**, **Comparison** and **Report Task** with the nearby terms they
   must not be confused with (Evidence, Demand, Document).

Acceptance:

- `af catalog init --profile report` produces a directory whose Task runs to `verified` with
  three command Attempts and no credential, and `af task output` writes its Markdown.
- The author's sandbox seals byte-identical to its source; an author that edits its source fails
  its Attempt with the ADR-0118 message.
- Every report receipt names the source Snapshot its citations were checked against, and no
  report is accepted whose check, verifier and acceptance name different Snapshots.
- A report Task cannot be delivered; a Document Task and an implement Task are unchanged,
  proven by byte-identical existing fixtures.

### R4 — Bind any declared root port

**Depends on:** R2 and R3 (the ports worth binding exist).

Deliverables:

1. ADR-0117's binding rule is widened: a Task file's `inputs` table may bind any root input port
   the selected Pipeline declares, when the referenced Task is recorded and `Finished`, the
   named output is in its result's `outputs`, every artifact verifies in the CAS, and the
   recorded type and cardinality equal the port's exactly. `requirements`, `base` and
   `continuation` stay refused by name for the reasons ADR-0117 gives. `source` keeps its
   re-rooting rules unchanged. Only result outputs bind: an Attempt's `raw_artifact_ids`, runtime
   evidence and other non-output records are not bindable, and a Task file that names one is
   refused with a message saying so. A `one` port bound from one output keeps that output's
   Snapshot ID; a `many` port bound from several outputs — `measurements` from an experiment's
   `baseline` and `candidate`, which measured two Snapshots — carries no Snapshot ID on the
   port, because `af/ArtifactInputV1` names one, and each artifact keeps its own subject
   Snapshot in its envelope. A `many` port bound from a single output keeps that output's
   Snapshot ID. Binding several outputs into a `one` port is refused.
2. `af/TaskInputBindings@1` records every binding as today; `af task explain` and `af task
   show` display them unchanged.
3. A three-Task fixture in one Store: the R2 experiment Task; a report Task binding
   `comparison`, `measurements` (from the experiment's `baseline` and `candidate` outputs) and
   `source` to it and running to `verified`; and a report Task binding `sources` to a Document
   Task's `document` output, refused at plan time because the types differ.
4. One ADR amending ADR-0117 (status note on 0117's line), a `CHANGELOG.md` entry, and the
   updated `docs/task-execution/task-inputs.md`.

Acceptance:

- A bound `comparison` and both bound `measurements` reach the report author and verifier as
  exact artifacts, proven by the fixture's context manifests; the `measurements` port carries no
  Snapshot ID and each Measurement its own.
- Every refusal is raised before any Worker or Provider admission and names the port and the
  type mismatch.
- A Task file without `inputs` keeps byte-identical revision, plan and inspection documents.

### R5 — Store hygiene: `af task gc` and sizes

**Depends on:** nothing; sequenced last because it changes no research behaviour.

Deliverables:

1. `af task list --sizes` prints, per Task, the bytes of CAS objects only that Task reaches and
   the bytes it shares with other records, plus the Store total and object count; `--json`
   carries the same numbers. Reachability is the transitive walk of deliverable 3.
2. `af task gc --state DIR --older-than DAYS --keep N [--apply]` previews and, with `--apply`,
   collects finished Tasks beyond the newest `N` whose last event is older than `DAYS`. A Task
   that is `Running`, holds a writer lease, or is named by another Task's
   `af/TaskInputBindings@1` is never collected, and the preview says so. Without `--apply`
   nothing is written.
3. Collection is one versioned transition: the command takes an exclusive Store lease that no
   live writer holds, appends one `task_collected` event carrying `af/TaskCollected@1` — the
   Task's ID, kind, revision ID, outcome, chargeable tokens, last event time, collection time
   and the byte total the preview computed — and only then sweeps. The sweep walks every record
   in the Store that is not collected (every Task's revision, plan, execution records, results,
   `raw_artifact_ids`, input bindings, delivery records, and every Campaign record the Store
   holds) transitively through the artifacts they reference, and removes each CAS object the
   walk did not reach. A collected Task's tombstone references no artifact, so its objects
   become unreachable unless another record reaches them. If the process stops between the
   tombstone and the end of the sweep, the next `gc --apply` finishes the sweep from the same
   reachability rule; a tombstoned Task whose objects still exist is consistent, never corrupt.
4. A collected Task's projection stops artifact validation at the tombstone: `af task list` and
   `af task show` report `collected <time>` with the retained summary instead of the
   artifact-backed sections, `af task output` and `af task deliver` refuse with the same word,
   and replay never reports a missing artifact of a collected Task as corruption.
5. Tests and fixtures: two finished Tasks, `--keep 1` collecting the older one, its exclusive
   objects gone, the shared objects and the retained Task's `show` byte-identical; a running
   Task refused; a Task named by a binding refused; a sweep interrupted after the tombstone and
   completed by the next run; a Store with a Campaign whose records reach an object a collected
   Task also referenced keeping that object; `gc` without `--apply` writing nothing.
6. One ADR (options rejected: deleting a Task's events; a reference count kept in the CAS;
   `af review gc` semantics of removing whole directories), a `CHANGELOG.md` entry, and a
   paragraph in `docs/tasks.md`.
7. The warm cache's two bounds (added after R2, whose implementation Attempt the single bound
   ended 61 s into its gate for growth the candidate did not cause): `[warm] max_bytes` stays the
   eviction bound — a directory above it is removed *after* the check, whose result stands, and
   before the next one — and a new `[warm] hard_max_bytes` (default twice `max_bytes`, at most
   the declared value) is the only bound that ends a running check, with the same
   `WARM_CACHE_BOUND_EXCEEDED` reason. `TaskCacheObservationV1` records which bound acted. The
   pre-check and post-check rules of ADR-0123 are otherwise unchanged, and ADR-0123 is amended.

Acceptance:

- A check that grows its directory past `max_bytes` but not `hard_max_bytes` passes on its own
  result and finds the directory removed before the next check; one that passes
  `hard_max_bytes` is ended and fails, as today.
- After `gc --apply`, every retained Task's `show --json` document is byte-identical to before.
- The Store's byte total drops by exactly the previewed amount on the fixture.
- No command other than `gc --apply` ever removes a CAS object.

### R6 — First research Tasks (campaign closure, no package code)

With R1–R4 delivered and the kernel built from the branch, two research Tasks run against this
repository and their spend and outcome are recorded in §6. Neither Task asserts an improvement:
the declared objective and the recorded comparison are the result.

1. **Release build.** An experiment Task on `kernel/experiment` using the committed
   `release_build` measure and `release_build_time` objective from R2. The requirements state
   that the implementer may change `Cargo.toml` profiles, feature flags and
   `scripts/measure-release.sh`'s build invocation and nothing else. Every repetition's receipt is
   retained; the exact medians, the comparison rule and any `inconclusive` are the outcome.
2. **Cycle time and disk.** A report Task on `kernel/report` whose author reads this
   repository, with `comparison` and `measurements` bound to the release-build Task through
   R4, and with `report_sources` holding the `af task show --json` documents of every Task of
   this campaign, exported by the coordinator into the sources file (one entry per Task, each
   under 256 KiB, the file under 640 KiB). It answers where the cycle's time and disk go, using
   the recorded check spans and cache observations, and which change this plan should make
   next.

If either Task fails, the failure and its evidence are the result; no number is invented.

## 5. Validation and rollback discipline

Each package is one Task with its own reviewers and evaluator, delivered to its own worktree and
verified there by a verification Task before the next package captures it. A package that fails
review or verification is fixed in its worktree by a human or an agent and re-run as a new Task
revision. Anything unverified is not merged. The design itself is reviewed once, by one GPT-6 Sol
(high) reviewer over the `design-review` Pipeline, before R1 is planned; its Findings are fixed in
this document and recorded in §6.

## 6. Execution record

### Design review

Campaign `research-design` (Pipeline `design-review`, one GPT-6 Sol high reviewer, Codex) over
the design commit d4531f5 against 4097093: 5m43s, 238,045 tokens, 16 Findings (10 blockers, 6
majors) and two required Demands. Every Finding is fixed in this revision of the note:

| # | Finding | Disposition |
|---|---|---|
| 1 | Persistent build cache crosses ADR-0108's trust scope | §2 and R1.7: an explicitly unsafe Warm Check Cache with admission, validation and invalidation rules; the R1 ADR supersedes ADR-0108's one-Round scope in part, for checks only |
| 2 | Toolchain declaration bytes are not a toolchain identity | R1.2: key includes `rustc -vV`, `cargo -vV`, target triple and the fixed check environment |
| 3 | Post-check removal does not enforce the byte bound | R1.3: bound enforced before, during (monitor, supervised kill, `warm_cache_bound_exceeded`) and after the check |
| 4 | Warm and cold inspection JSON cannot be byte-identical | R1 acceptance: byte identity for Snapshots, candidates, delivery and outcomes; `runtime_observations` differ accurately; no-`[warm]` Tasks byte-identical |
| 5 | The speed ratio is an unstable fixture gate | R1 acceptance: deterministic fixtures for selection and reuse; the speed claim is benchmark Evidence recorded here (Demand 1) |
| 6 | The measurement line cannot report a wrong unit | R2.3: every reported value carries its unit; `unit_mismatch` fails the measurement |
| 7 | Comparison arithmetic lacks zero and decimal rules | R2.3–R2.4: canonical decimals, even-sample median, signed improvement, zero-baseline rules, strictly positive improvement for `passed` |
| 8 | Read-only measurement conflicts with the required commands | §2 and R2.2: read-only source as working directory plus a private writable runtime directory, source re-verified after every repetition |
| 9 | Repetition limits define no aggregate wall bound | R2.1–R2.2: compile-time fit against `check_wall_ms`; deadline and timeout are `failed` reasons, never a partial measurement |
| 10 | Optional and enlarged sources conflict with `DocumentSources@1` | R3.2: `af/ReportSources@1` (0–256 entries, 4 MiB); the Document profile is unchanged |
| 11 | Repository citation coordinates are underspecified | R3.4: `encode_path` spelling, regular file, no NUL in the first 8 KiB, `line` in range |
| 12 | Report acceptance is not bound to its source Snapshot | R3.5: `report_seal`, `report_check` and `report_accept` with `ReportCheckReceipt@1` and `ReportVerification@1` retaining the Snapshot; `same_as` affinity throughout |
| 13 | R4 binds a port the report profile never declares | R3.1: optional `comparison` and `measurements` root ports on the report profile, wired to author and verifier |
| 14 | Collection lacks a replay-safe reachability rule | R5.3–R5.4: `af/TaskCollected@1` tombstone, exclusive lease, Store-wide transitive walk, crash recovery, projection stops at the tombstone |
| 15 | R4 cannot bind raw Task runtime evidence | R4.1 refuses non-output records by name; R6.2 captures `af task show --json` documents into `report_sources` instead |
| 16 | The release experiment has no committed measure | R2.7: `release_build` measure, `release_build_time` objective and `scripts/measure-release.sh` committed by R2 |

Demands: (1) the warm-check speed claim is satisfied by the paired cold and warm checks R1's
acceptance now requires this record to hold; (2) the release-build improvement claim is
withdrawn — R6 declares an objective and records whatever the comparison concludes.

### Packages

#### R1

Task `research-r1` (plan `sha256:c7835d9b…`, source 34a3e4e) was started while another session
was running a Task on this machine and stopped with `SIGTERM` before any Worker had replied. The
implementer's Attempt had already been reserved, so on resume the node had "exhausted its
Attempt limit" and the Task ended `incomplete`; the ledger charged that interrupted Attempt its
full 1,500,000-token reservation (ADR-0068 accounting, not Provider spend). Rule learned: look
for other `af task run` processes before launching, never interrupt a running Task to serialize.

Task `research-r1b` (plan `sha256:cdecf975…`, same source): implementer (Claude Opus 5.5, high,
with a shell) completed one Attempt of 431,817 chargeable tokens and sealed every deliverable it
could reach — the `[warm]` policy table, the toolchain-keyed locked cache directory, the
three-point byte bound with a sampling monitor, the cache observations and the `af task show`
line, `scripts/verify.sh`, the fixtures, ADR-0123 and the changelog entry. It reported two
deliverables it could not do: the `[warm]` declaration in this repository's `.af/code-policy.toml`
(a Worker may not edit `.af/`) and the paired benchmark (it may not run `make check`). The gate
passed on the first try: `kernel` 15.3 min cold, `markdownlint` 0.3 min. The `bugs` reviewer
(GPT-5.6 Terra) returned malformed JSON and its slot had one Attempt, so the Round ended
incomplete and the Task `changes_requested` at 723,510 tokens; `kernel/review-code` now gives
each reviewer two Attempts. The `correctness` reviewer (GPT-6 Sol) returned five majors, all
fixed by hand on the materialized candidate (commit 769b11a): the missing policy declaration; a
link *below* the cache root was reused, so `WarmDirectory::ensure` now walks the whole directory
without following links and removes it on any link, special file or foreign owner; warm
preparation could outlive the Attempt deadline, so the probe and the lock wait are bounded by the
remaining time, the check's timeout is recomputed after preparation, and exhausting it records
`not_run`; an eviction produced a second observation for one kind, so it is now the
`evicted_bytes` field of the kind's one observation; and the benchmark evidence was absent. The
benchmark harness is `kernel/gate-bench`: the repository's checks and a command evaluator that
passes when they passed, so paired cold and warm gates run without a model.

Verification Task `research-r1-verify` on a3c4511 (270,563 tokens) ended `changes_requested`
and surfaced the defect that decides whether R1 does anything on this machine: its gate passed
(`kernel` 15.7 min) but recorded `cargo_target cold toolchain_unresolved` — the probe's
`rustc -vV` exceeded its 30-second bound. Reproduced by hand: with the check's fresh `HOME`, the
rustup proxy `rustc` reports "syncing channel updates … downloading 5 components" and installs
the pinned 1.88.0 toolchain into that `HOME` before answering. Every Task check on this machine
has therefore been downloading a Rust toolchain and the whole crate registry into a throwaway
directory and then compiling cold; the 15-minute gate is that, not only compilation. R1.1, R1.2
and R1.5 are amended above: under `[warm]` the kernel's rustup home is bound for the probe and
the check with `RUSTUP_AUTO_INSTALL=0`, and a second warm kind, `cargo_home`, keeps the registry
beside the build directory. The reviewers added six defects — a fast over-bound check passes
because the monitor samples every five seconds; an unreadable subdirectory is skipped by the byte
walk and the suspect walk; a suspect directory that `ensure` discards is still reported warm with
its old bytes (both reviewers); a check that never starts loses its evidence or its name; and the
paired benchmark is absent — and the evaluator asked for the byte-identity proof of a Task
without `[warm]` and a sandbox-manifest proof that no cache byte reaches a Worker. All of it is
the scope of Task `research-r1c`; the paired benchmark follows it.

Task `research-r1c` (plan `sha256:6f99c787…`, source b77116a): the implementer completed one
Attempt of 503,000 chargeable tokens and delivered the whole amendment on top of the R1 code —
`RustupHome::of_kernel()` and the two rustup variables for probe and check, the `cargo_home`
kind with its own lock and the shared bound, the post-execution measurement that catches a fast
over-bound check, descriptor-relative fail-closed traversal, measurement after validation, a
named evidence group per check whether it ran or not, the schema and ADR amendments and eleven new
unit tests — and reported the one `.af/` line it may not write. The gate passed (`kernel`
15.1 min, still cold: the kernel running the Task predates the amendment). Both reviewers replied
this time: `bugs` (Terra) two majors and `correctness` (Sol) two majors, three distinct defects —
the shared bound was not serialized across checks declaring different kinds; a credential a check
writes into its `cargo_home` survived a passing check; a root swapped for a link during a fast
check escaped the final count. The evaluator failed the Task only on the missing policy line. The
Task charged 835,692 tokens. Fixed by hand on the materialized candidate: one exclusive lock over
the whole toolchain key held from preparation to removal, the bound measured over the whole key,
every used directory judged suspect after the check exactly as before it (reason
`warm_cache_suspect`, removal under the lock), and a non-directory root counted as uninspectable;
three unit tests pin them, and `.af/code-policy.toml` declares both kinds.

The first paired runs on that tree taught two more things, both recorded here rather than argued
away. The default 8 GiB bound is smaller than one `make check` of this workspace: the first warm
gate started at 8.2 GB and was evicted 35 seconds in at 8.63 GB, exactly as designed, so this
repository's policy now sets `max_bytes` to 16 GiB. And a warm target directory hands the next
gate test binaries that were compiled in the previous gate's sandbox, so any test that bakes a
fixture path in at compile time — `env!("CARGO_MANIFEST_DIR")` without the run-time
`AF_WORKSPACE_ROOT` fallback `scripts/verify.sh` exports for exactly this — looks for a destroyed
directory: two tests in `review-config` failed the first warm gate (8.9 min, versus 14.0 cold,
before failing). Nine such paths were made run-time and a test now refuses any new one.

**Paired benchmark** (Demand 1). Five cold/warm pairs of this repository's checks on one pinned
Snapshot (commit d7ba8d8, toolchain key
`d7e5ef8d…`, rustc 1.88.0) through the credential-free `kernel/gate-bench` Pipeline, one Task
per gate, run one at a time on a MacBook on AC power, 2026-09-28 01:00–03:12 local. Cold means the
whole `task-build-cache` directory was removed before the gate; warm means the previous cold
gate's directories were reused (`cargo_target` 8.2 GB, `cargo_home` 109 MB, both `eligible`).
The span is the `kernel` check's recorded `Check` span, which runs `scripts/verify.sh` →
`make check`.

| Pair | Cold `kernel` span | Warm `kernel` span | Ratio | Cold / warm `markdownlint` span | Load (1 min) cold / warm |
|---|---:|---:|---:|---:|---|
| 1 | 13.8 min | 11.9 min | 0.86 | 15.6 s / 15.8 s | 16.1 / 3.7 |
| 2 | 13.9 min | 12.0 min | 0.86 | 15.8 s / 15.8 s | 4.3 / 9.8 |
| 3 | 13.8 min | 11.9 min | 0.87 | 15.9 s / 16.4 s | 4.8 / 4.0 |
| 4 | 13.8 min | 12.0 min | 0.87 | 16.4 s / 15.9 s | 3.8 / 2.9 |
| 5 | 13.8 min | 12.0 min | 0.87 | 15.3 s / 16.2 s | 3.6 / 4.1 |

Median ratio 0.87, spread 0.86–0.87; cold spans 13.8–13.9 min, warm 11.9–12.0 min; the
`markdownlint` check, which builds nothing, is 15–18 s in both states. The
`make check` steps explain the shape: cold `lint` 41–47 s and `test-build` 49–52 s become warm
20 s and 13–15 s, while `test` runs 728–732 s cold and 672–679 s warm, and `fmt` and
`release-resolution` are 1 s and 5 s in both. A warm check removes almost all compilation, about
two minutes of fourteen; the other twelve are the test suite executing under
`--test-threads=4`. Warm checks are therefore worth having — every package's gates and every
verification Task pay them — but the check time this campaign set out to research is test
execution, not compilation, and that is the first hard fact for R6's report. The earlier 15–16
minute gates included the toolchain download the rustup binding removed.

Verification Task `research-r1-verify-2` on 6dc2c56 (301,446 tokens; its own gate ran warm in
12.0 min) ended `changes_requested` with one blocker and four more defects, all fixed by hand: a
check could rename the toolchain key's parent and plant a link in its place, and `remove_dir_all`
through the saved path would have followed it out of the cache, so every operation below the key
now goes through the key directory's open descriptor and never a path, with a parent-swap test;
the pre-check bound summed only the kinds the current policy declared, so a key another policy
left over the bound was never evicted and an eviction removed only the held kinds, so the bound is
now measured over the whole key before and after every check and an eviction empties the key of
every kind, with a cross-policy test; `af task show` called every eviction a bound violation, so an
observation now carries `evicted_reason` beside `evicted_bytes`; and the record above gained the
`markdownlint` spans the evaluator asked for.

Verification Task `research-r1-verify-3` on 37b9ea8 (305,432 tokens; gate warm, `kernel`
13.5 min under load) ended `changes_requested` with four defects, again all real and all fixed by
hand: a policy whose only kind a Cache Snapshot superseded skipped the key lock and the whole-key
bound, and its observation lost the toolchain identity, so every declared kind now resolves the
toolchain and holds the key; a check that deleted its own warm root and exited successfully was
detected as suspect but still passed because nothing was left to evict, so any excess now fails the
check and every declared kind records the cause; and the key's entry listing exempted every file
ending in `.lock`, so a check could park an oversized `extra.lock` beside its directory forever,
so only the kernel's exact lock files are exempt, they are truncated on acquisition, and only the
two known kinds are ever locked. Each has a unit or end-to-end fixture.

Verification Task `research-r1-verify-4` on fd213c3 (338,309 tokens; gate warm, `kernel` 12.4
min) ended `changes_requested` with three more in-key bypasses, fixed by hand: a policy whose only
kind was superseded held the key but was not monitored after the check; lock files were exempt by
name, so a check could unlink the held lock and park an oversized file at its name, or fill a kind
lock nobody held; and a widened key directory was repaired and reused on the next acquisition. Now
only the lock inodes this holder opened are exempt, the key is judged before the check is accepted
and emptied on acquisition when it is not private, and a held key is monitored whether or not a
kind is bound. Each has a unit and an end-to-end fixture. Four verification rounds and eleven
hand-fixed defects on the warm cache are themselves a finding for R6: reviewers with a shell hold a
`trusted_local` cache to an adversarial standard the design words invited, and the standard was
worth meeting — every bypass they found was real.

Verification Task `research-r1-verify-5` on 2771c0a (288,947 tokens; gate warm, `kernel`
12.5 min) ended `changes_requested` with five more, fixed by hand: a hard link to a held lock's
inode anywhere in the key escaped the count; bytes written into the held lock itself escaped it; a
key made read-only by a check defeated its own eviction; an entry with a non-UTF-8 name survived
eviction under a lossy spelling; and a Cache Snapshot observation missed the eviction cause. A held
lock is now exempt only at its own name with its own inode and must stay one empty name, the key is
made writable through its held descriptor before an eviction that addresses entries by exact bytes
and errors when anything remains, and every declared observation carries the cause. Two unit and
two end-to-end fixtures pin them.

Verification Task `research-r1-verify-6` on 874944d (360,806 tokens; gate warm, `kernel`
12.6 min) ended **`verified`**: the independent evaluator passed every R1 requirement, the fixtures,
the policy declaration, ADR-0123, the changelog and the benchmark record. The two reviewers still
reported four in-cache findings — one of them a real safety defect this campaign introduced:
acquisition truncated whatever inode sat at the lock's name, so a hard link a check planted to a
file outside the cache would have been emptied on the next gate. Fixed by hand before delivery: a
lock's inode is judged a plain, singly linked file of this user at its name before anything writes
through it; the project level is held open so a key a check renamed and recreated is displaced and
suspect; eviction empties or drops the held locks and succeeds only when nothing but sound empty
locks remains; and suspicion outranks the byte count in the recorded cause. ADR-0123 now also
states where these rules stop: `trusted_local` is not isolation, and the cache is honest as
evidence, not a defence against a check acting on the host. Six verification rounds and fifteen
hand-fixed defects on one package is the price of a shell-bearing reviewer holding the design's
words to their letter; it is also the strongest evidence this campaign has that the pipeline works.

Verification Task `research-r1-verify-7` on 334ee7a (321,723 tokens; gate warm, `kernel`
12.6 min) ended `incomplete`: the `bugs` reviewer (GPT-5.6 Terra) returned malformed JSON on both
of its Attempts — its third malformed reply in this campaign — so the Round could not close. The
`correctness` reviewer (GPT-6 Sol) added two acquisition defects, fixed by hand: a waiter that
validated the lock's inode and then waited on another holder truncated it after acquiring without
judging it again, and a link an interrupted check left at a lock's name made `openat(O_NOFOLLOW)`
fail before the recovery path ran. The lock's name is now cleaned before it is opened and the inode
judged again after the wait. The `bugs` package moves to GPT-6 Sol at high for the rest of the
campaign; Terra's replies were the only reviewer failures the kernel saw.

Verification Task `research-r1-verify-8` on 1048a12 (265,270 tokens; gate warm, `kernel` 12.9 min,
`cargo_target` 12.0 GiB, `cargo_home` 104 MiB) ended **`verified`**: the evaluator (GPT-6 Sol)
passed the package against every acceptance criterion. The two reviewers, both GPT-6 Sol now,
still reported four findings, none of which changed the verdict. Three were fixed by hand on top
of the verified commit, each with a unit test, and are not re-verified by af: a held lock a check
grew was exempt from the running bound (it counts now); `remove_at` spelled a child's name lossily
and could remove the wrong one of two colliding entries (removal is by exact bytes now); and the
`source_digest` of a directory carried the lookup's outcome (it names the directory alone now).
The fourth — a check that unlinks the lock it holds and plants another entry at its name lets the
next check recover onto a fresh inode while the first still runs — is recorded in ADR-0123 as
outside these rules: it is the check acting against the cache it was trusted with. R1 closes here.
Eight verification Tasks cost 2.45M tokens against 1.56M for the two implementation Tasks that
count; the verification loop, not the implementation, is what this package's evidence says to
bound next.

#### R2 — Measure and compare

Implementation Task `research-r2` from 318f64b (kernel/implementation-reviewed; 753,016 tokens;
4 Attempts) ended `changes_requested` without a reviewer reading a line: the implementer (Claude
Opus 5.5) delivered the whole package and reported `cargo test --workspace` and clippy green, but
the gate's `kernel` check was ended 61 s in by the warm cache's 16 GiB bound. The `cargo_target`
directory held 13.5 GB after R1's eight verifications — cargo keeps every earlier revision's
artifacts — and a candidate that touches review-core added 4.5 GB while compiling; the kernel
removed 18.0 GB and failed the check, as ADR-0123 says it must. That is the design working as
written and the wrong outcome for a research pipeline: a bound tuned to the first tree ended an
implementation Attempt for a reason the candidate did not cause. The bound is raised to 32 GiB
against 98 GiB free, and bounding the running check separately from evicting an over-bound
directory after it is recorded as a follow-up for R5. The candidate was materialized from its
Snapshot by hand; `.af/` cannot be written by a Worker, so the `release_build` measure, the
`release_build_time` objective and the three `kernel/experiment*` packages the implementer staged
under `fixtures/kernel-experiment/` were installed by hand as its README says. Six deviations
from this section's wording were accepted and the section amended: the `below_threshold`
conclusion, the ratio as an exact fraction, cache bytes per repetition, the child Pipeline that
gates the evaluator on two receipts, one node at a time, and a bound-ended repetition recorded
as `exit`.

Verification Task `research-r2-verify-1` on ff88a5d failed at its gate (2,151 tokens) before any
reviewer ran: two TUI unit tests panicked with an event-store clock conflict inside the af
binary's test run, and both pass in a local `make check` of the same tree. That flake is recorded
in the repository's release notes as a gate-only failure and the Task was simply re-planned as
`research-r2-verify-2` on the same commit, which passed its gate warm (`kernel` 13.1 min) and
ended `changes_requested` (354,097 tokens): both reviewers (GPT-6 Sol) found that a repetition the
kernel ended at its wall was recorded as `exit`, because the cancellable supervisor the Task runs
under reports a timeout with a different shape than the predicate expected; the correctness
reviewer found that a `warm = true` measure whose key was busy still recorded `warm: true`; and the
evaluator failed the package on one criterion — `min_improvement_ratio` deserialized a TOML float
through f64 and captured its rounded spelling, so `0.10000000000000001` became `0.1` and an
improvement of exactly one tenth would have passed. All three were fixed by hand with tests: the
runner now carries a typed ending beside its result and the operator classifies the bound from it;
each run records the cache condition it actually had and `af task show` prints it; the ratio is
decimal text or the integer 0 or 1, and every policy in the repository writes `"0.1"`. ADR-0124
records the three amendments.

Verification Task `research-r2-verify-3` on afb7fd2 (330,318 tokens; gate warm, `kernel` 13.8 min)
ended **`verified`**: the evaluator passed the package on every acceptance criterion. The two
reviewers still reported two majors, both fixed by hand after the verdict with tests and not
re-verified: a command a signal ended recorded the runner's `-1` sentinel as its exit code (a run's
exit code is now the command's own or absent), and a warm directory the kernel discarded before
binding was recorded as warm (the run now says `cache.warm = false` with `discarded: <why>`, and
the repetitions after it, which find the directory warm again, read `warm 2 of 3` in `af task
show`). R2 closes here. Its three verification Tasks cost 686,566 tokens against 753,016 for the
one implementation Task; the first was a gate flake that cost 2,151.

#### R3 — Report Tasks

Implementation Task `research-r3` from 0c72e63 (753 lines of design in scope; 1,067,770 tokens; 7
Attempts; gate warm, `kernel` 14.2 min) ended `changes_requested`. The implementer (Claude Opus
5.5) delivered the profile, the three operators, both artifacts' schemas, the staged
`kernel/report` packages with an idempotent install test, ADR-0125 and the docs, and reported the
whole workspace green. Both reviewers (GPT-6 Sol) and the evaluator converged on one contract
defect: the `sources` root port was declared required although this section says optional; the
Task-file adapter's empty set hid it. The reviewers added three more: an execute-checks author
could add a file under an existing source directory and still pass the byte-identical seal
(ADR-0118's rule ignored every addition); a sources file over 4 MiB passed capture because only
its text was counted; and a valid 4 MiB sources set could never reach a Worker through the 1 MiB
request. All four were fixed by hand with tests, and the third changed this section: sources are
bounded at 512 KiB of text in a 640 KiB file, and bounded retrieval of larger sources is recorded
as a follow-up. The implementer also found that a Worker port carries one Snapshot ID, so R4's
`measurements` binding of a baseline and a candidate from two Snapshots cannot render as written;
R4 amends its section before it runs. The staged `.af/` packages were installed by hand as their
README says.

Verification Task `research-r3-verify-1` on 62aee50 failed at its gate (2,163 tokens; `kernel`
14.2 min): the tightened source-edit rule broke an R1-era kernel test whose fixture reviewer
added `src/extra.rs` and expected acceptance — the very behaviour the review had called a defect.
The fixture now adds beside the source, and an addition under an owned directory is a refused
case. Planning this repository's own report Task with the installed packages was infeasible:
`kernel/report` declared `max_attempts = 3`, which cannot hold an author, a verifier and their two
Provider admissions; the repository test passed because its command stand-ins admit no Provider.
The bound is 6, as `kernel/experiment`'s. Both fixed before the second verification.

Verification Task `research-r3-verify-2` on 201d498 (347,598 tokens; gate warm, `kernel` 12.9 min)
ended `incomplete`: the evaluator (GPT-6 Sol) passed the package on every criterion and the
correctness reviewer raised no claim, but the `bugs` reviewer's reply put its findings on a
`result` port of the wrong shape twice, and the kernel refused both Attempts as an undeclared
port. Its transcript still held one real finding: acceptance required a captured `sources` input
and would have refused any report whose Pipeline bound nothing to the optional port. Fixed by
hand — acceptance accepts such a report only against the empty set the seal recorded — and the
Task re-verified once more. A reviewer reply the kernel refuses for its shape is the second such
loss in this campaign (Terra's malformed JSON in R1 was the first); the kernel could hand a shape
refusal back to the same Attempt as feedback instead of spending a fresh one, which is recorded
as a follow-up.

Verification Task `research-r3-verify-4` on af18c14 (304,910 tokens; gate warm, `kernel` 12.6 min)
ended **`verified`**: the evaluator passed the package on every acceptance criterion. Both
reviewers still reported three majors, all in the optional-sources rule added by hand after the
first review, and all fixed by hand after the verdict with tests, not re-verified: the empty set
the seal records was not among the check receipt's references, so a report over an unbound port
could never admit its verifier; the same path lacked an end-to-end test through admission and
acceptance; and a verifier could omit its `sources` input while its checks had judged captured
text. R3 closes here. Three verification Tasks that ran cost 654,671 tokens against 1,067,770 for the one
implementation Task; the first was a gate failure of a kernel test the review itself had made
stale, the second a reviewer reply the kernel refused for its shape.

#### R4 — Bind any declared root port

Implementation Task `research-r4` from de3bf63 (733,989 tokens; 7 Attempts; gate warm, `kernel`
14.6 min) ended `changes_requested` with the evaluator passing every criterion: the implementer
(Claude Opus 5.5) delivered the widened binding rule with its refusals, the list form for `many`
ports, the two-Snapshot exception, ADR-0126 amending ADR-0117, and the three-Task chain fixture
that runs an experiment and then a report bound to its comparison and both Measurements to
`verified` in one Store. The reviewers (GPT-6 Sol) left one major and one minor, both fixed by
hand: a port bound without naming the Pipeline was accepted when only some Pipelines accepting the
kind declared it, so selection would have let the binding choose the Pipeline; and a list bound to
a `one` port was refused for its shape before its references were judged, hiding that a name was
not a result output. The implementer also reported the one change it could not make: the
installed `kernel/analyst` and `kernel/report-verifier` schemas lacked the `snapshot_id` a bound
Measurement always carries, so R6's report could not have bound R2's outputs to them; both
schemas were amended by hand, reinstalled and repinned.
