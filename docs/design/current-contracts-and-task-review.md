# Current contracts and native Task review

[Decision](../adr/2026-11-08-consolidate-current-contracts-and-task-review.md).
Afactory has one current Task format per concept and one native Task execution path for Review.
`af review` plans, executes, inspects and continues native Tasks; it has no Campaign execution
fallback. Shared Ledger reducers and canonical `review.kernel/*` artifacts retain their domain
identities. Telemetry observes committed Task publication and never decides acceptance or recovery.

## Current formats and ownership

| Concept | Current representation | Owner and proof |
| --- | --- | --- |
| Task Pipeline | `af.pipeline/6` | `af-contracts/src/task/pipeline.rs`, `af-pipeline-config/src/task/`; compiler and catalog tests retain typed ports, Brief authority, recovery and structural acceptance. |
| Compiled graph | `af.compiled-task/6`, `af/CompiledTask@4` | `af-graph/src/task.rs`, `af-pipeline-config/src/task/catalog.rs`; exact recompilation and closed-graph tests retain every port, condition and resource bound. |
| Task kind | `af.task-kind/3` | `af-pipeline-config/src/task/kind.rs`; one format with explicit optional capabilities, with retired-version refusal. |
| Draft | `af.document-draft/3`, `af/DocumentDraft@3` | `af-contracts/src/task/document.rs` and the common renderer. One typed citation set distinguishes captured data from repository locations; Document Tasks refuse repository citations. No ReportDraft conversion remains. |
| Review Subject | `af.task-review-subject/3`, `af/TaskReviewSubject@3` | `af-contracts/src/task/review_context.rs`, runtime Review context validators; exact Base/head, complete Change Set, rename linkage, scope and prior history remain checked. |
| Source origin | `af.task-source-origin/3` | `af-source-git/src/task.rs`; `captured` retains commit or dirty-tree provenance, while `bound` requires admitted artifact/Snapshot and optional producing Task/output. No older DTO is constructed. |
| Review history and Round | `af/ReviewHistory@1`, `af/TaskReviewRound@1` | `af-runtime/src/task/review/`, checked original plans and native publication; complete required siblings and exact cumulative accounting precede authoritative Ledger publication. |
| Store events and inspection | `review.kernel/RunEvent@3`, `af/TaskTransition@6`, `af/task-inspection@20` | Closed current Store and Task contracts, their writers/readers and schema parity. Retired Campaign bootstrap and execution unions fail closed. |
| Project authority and model display | `.af/af.toml` version 2, `af/provider-models@4` | Native Task defaults, onboarding and doctor; model output exposes current Worker and binding templates. Release pins remain separate. |
| Benchmark output and registry | `af/telemetry-overhead@3`, `af/telemetry-invocation-cost@3`, `af.perf-telemetry-lab/4` | Python bench writers/readers, current schemas and `test_telemetry_bench_contracts`, `test_contracts.TelemetryLabVersion`; earlier output and registry shapes are refused. |

Crate paths in tables are below `crates/`. The schema directory is the language-neutral boundary;
Rust names do not themselves authorize another representation. Changed typed shapes have new
identities, coherent writers/readers, fixtures and parity. There is no migration or reader fallback
for retired execution state. The compiled schema's title names its actual `CompiledTask@4` envelope.

The published inventory has 417 JSON files, 414 schema IDs and three conformance corpora without
`$id`. The existing `af-contracts::schema_parity` registry enforces current consolidated ownership and
checks every published schema's resource and JSON Pointer reference closure, including Python-owned
schemas. `current_contract_owners_and_retained_observation_schemas_are_explicit` checks the exact
current families, Python writer identities and the retained coexisting versions. Python performance
parity separately compares every generated schema against its actual reader definition and exercises
positive and negative fixtures. This extends the existing registry rather than adding another one.

Only four lower-version schema files coexist with a newer member of their family:

| Retained file | Concrete current consumer |
| --- | --- |
| `performance-executor-request-v1.json`, `performance-executor-request-v2.json`, `performance-executor-result-v2.json` | `scripts/performance/perflab/executor_contracts.py` admits immutable journal originals for status and verification. `test_executor` and performance parity retain reader/writer and refusal controls. New originals use request/3 and result/3. |
| `provider-models-v2.json` | `af/tests/it/catalog_digest.rs` validates captured raw/projected Provider observations and their exact content digests before reproducing model support. Current CLI output is @4. |

Current versioned domain boundaries, Provider capability/independence checks and CAS artifact versus
content identity remain separate and required. Evidence and Satisfaction records remain useful native
contracts; their names appearing in canonical history do not make them obsolete Campaign readers.

## Native Review capabilities

The native path owns the following behavior. Missing required outputs do not supply a partial Ledger,
and a successfully executed human decision may legitimately retain `changes_requested` (exit 3).
Negative decision tests therefore require the specific refusal and absence of newly published
canonical Review authority, in addition to original-Task immutability.

| Retained capability | Current native owner and regression |
| --- | --- |
| One complete Round, missing siblings and recorded partial results | `af-runtime/src/task/review.rs`; `task_review::{a_clean_review_task_passes_without_a_partial_ledger,a_missing_reviewer_leaves_the_review_task_incomplete,an_unavailable_reviewer_leaves_the_review_task_incomplete}`. The reducer publishes no partial Ledger. `af review ledger ID --json` retains coverage and never invents a partial canonical Finding Set. |
| Complete Ledger inspection across Rounds | `af/src/task_commands/{review_ledger,findings}.rs` reads the full selected canonical Finding Set. `review_decisions::native_ledger_keeps_original_unresolved_findings_after_not_reproduced_and_dispute` executes CLI continuation and proves exact original IDs, open/contested status, original Report producer/Snapshot and correct counts in text, JSON and SARIF. The JSON is `af/review-ledger@2`; current-Round Task reports remain separate. |
| Admitted contained native checks | `af-runtime/src/task/code.rs` uses the shared `ContainerProvider` and `CheckRunner`, also through `ReviewTaskDomain`. `task_review_execute_checks::a_container_check_executes_on_the_native_task_host` requires a real usable runtime, satisfied acceptance and exact `TaskCheckContainment@1`/`TaskCheckReceipt@2` provenance, with failed-command, mutation and host-marker controls. The non-container missing-runtime regression proves refusal before checks/reviewers. `make review-kernel-container-probes` retains lower-level probes and uses an exact nextest selector with `--no-tests fail`; see the [live execution boundary](../security/containment-probes.md). |
| Exact original Finding/Report identity and dispositions | The shared canonical `af-store/src/ledger.rs` and native Review history; `task_review::review_scopes_dispositions_preserves_actual_producers_for_valid_dispositions`. Shared Ledger convergence and identity tests remain. |
| Report Scope authority and persisted conclusion explanation | `af-store/src/ledger.rs::{bind_task_subject,convergence}` and `af-runtime/src/task/review.rs::{current_subject,restore_round,reduce,assess}` refuse unavailable native authority and incomplete publication. `af-store::it report_scope::{unavailable_subject_is_refused_before_native_review,a_report_without_its_exact_round_subject_is_unknown_and_fail_closed,an_active_unreadable_report_blocks_after_its_original_clean_window}` cover scope/refusal; the native missing/unavailable tests above cover incomplete receipts. For standalone Review, `native_review_conclusion` writes `af/TaskReviewConclusion@1` under `schemas/task-review-conclusion-v1.json`, linking exact Round receipts and `TaskRunReport@3` node outcomes/diagnostics with checked prefix and cumulative charge, as exercised by the native accounting test below. There is no separate unavailable-authority reason field. |
| Source, Base, exact patch and continuation | `af/src/task_commands.rs::start_review`, native `ReviewBind`, `task_commands/input_bindings.rs`; `af/tests/it/task_file` covers independent authority/candidate/Base selection, exact readable patches, replay without dispatch and standalone/embedded Review. |
| Bound repair and heavy convergence | `af-runtime/src/task/review/{repair,continuation}.rs`, common Task history and captured policy; `af/tests/it/{task_repair,task_heavy,task_continue}.rs`. No new Round can reset the Task allowance. The same tests exercise the native path. |
| Exact accounting, late usage and recovery | Common Task Attempts, leases and durable outputs; `task_review::native::native_conclusions_freeze_exact_wide_usage_and_late_usage_fences_the_old_result`, Store wide usage/recovery tests, `native_integration_recovers_published_commit_without_repeating_proposals_or_checks`. No cumulative report snapshots are summed as independent charges. |
| Sealed proposals and checked Integration | `af-runtime/src/task/review/{proposal,integration}.rs`; `disjoint_native_proposals_promote_only_after_checks_and_keep_original_claims_pending`, `native_proposal_declaration_must_equal_the_entire_sealed_diff`, conflict, protected-path, failed-check, duplicate and manifest-change cases in the same native module. |
| Exact committed head and selected successor verdict | The `af-store/src/store/task/review_integration.rs` guards and native domain acceptance. The native successor/ancestry tests cover standalone, embedded, Select fallback, finish/replay and rejection of an earlier passing Round after a successor requests changes. Static reachability does not supply acceptance. |
| Imported SourceTree transport without a commit | `no_commit_transports_an_admitted_source_from_another_tasks_kernel_node` and `embedded_no_commit_keeps_its_exact_review_accept_snapshot_for_an_imported_source`; ordinary source transport is distinct from authoritative Review evidence. The same guards apply to imported source transport. |
| Slicing, owned children, lossless shards and whole-Subject closure | `af-runtime/src/task/review/scatter.rs`, shared owned-child dispatch and accounting; `native_scatter_runs_real_owned_attempts_and_closes_the_whole_subject`, `failed_native_shards_remain_in_the_complete_set_and_refuse_a_partial_ledger`, `unstarted_native_shards_remain_missing_without_spending_reserved_closeout_attempts`, closeout/coverage/bound cases in the same native module. |
| Terminal and agent inspection | `af/src/review_task.rs` delegates to common Task commands; TUI `tasks/` lists Review Tasks and their history. CLI help/schema/effects catalog tests and in-process `tui::` tests preserve navigation, exact attribution, read safety and renderer fixtures. |
| Human-directed Proposal export | `af review export ID PROPOSAL_ID` or `--finding FINDING_ID`; `task_file::native_review_facade_exports_non_nominated_repairs_and_refuses_false_original_claims` exercises a changes-requested Task, an un-nominated sealed repair, exact Finding lookup and false path/Report/result refusal. Export does not promote or publish. The same test covers actual committed stale bases, explicit stale export, credential-bearing authors and current-format archived-plan export. Durable admitted commit publications determine the current head; unrelated Round outputs do not. |
| Evidence attachment, exact satisfaction and explicit reuse | `af review decision-context ID` exposes original canonical identities; `af review decide NEW --from ID --file BATCH` captures a root-only deterministic Task. `af-runtime/src/task/review/decisions.rs` uses the canonical Ledger reducer and immutable Evidence/Satisfaction/Reuse receipts. `review_decisions::{human_evidence_satisfaction_reuse_and_waiver_are_native_zero_token_tasks,satisfied_evidence_becomes_stale_on_a_real_new_subject_and_requires_explicit_reuse,cross_demand_reuse_refuses_an_incompatible_measurement_method}` execute actual CLI Tasks and checked cold replay. |
| Required Demand waiver and human authority | The same exact-plan-approved Task records the original policy, Subject, actor and reason. No generic append is installed. `review_decisions::human_decisions_refuse_forged_authority_cross_demand_satisfaction_and_partial_batches` covers policy/actor/source/Base/identity/refusal and failure without partial publication. Original accounting and claims stay immutable. `waiver_remains_current_when_the_native_subject_advances` proves a real head change retains the original waiver and cold replay without replacement. |
| Rejected/tracked-wontfix resolution, material challenge and expiry | `ReviewDecisionV1::{Resolve,Challenge,PolicyTime}` and the shared canonical Ledger retain exact views, evidence, severity ceiling, tracking and expiry. `review_decisions::human_resolutions_challenges_policy_time_and_grouping_preserve_original_claims` executes rejection, challenge, tracked acceptance, monotonic policy time, automatic expiry challenge and cold reconstruction. |
| Grouping and ungrouping | `ReviewDecisionV1::Group` requires both current view identities and the original Round. The same CLI execution test groups and ungroups real Findings without rewriting their original Reports or identity. |
| Change attestation and positive/negative independent fix verification | Existing `af-runtime/src/task/review/{repair,continuation}.rs`, `AttestFixes`, reserved `FixVerify` and `RepairAccept` remain the only fixed-claim path. `task_repair::{bounded_repair_preserves_one_round_and_delivers_verified_s2,repair_rejects_a_negative_receipt,repair_rejects_a_missing_receipt,repair_rejects_a_stale_subject_receipt,repair_rejects_a_receipt_without_claims,repair_rejects_current_check_failures,repair_cannot_claim_full_review_omit_verifiers_or_bypass_reserved_budget}` execute it. Human decision batches explicitly refuse fixed-resolution shortcuts. |
| Native lifecycle telemetry and its reader boundary | `af-runtime/src/telemetry/mirror.rs` reconstructs complete native outputs and compares exact satisfaction/reuse identities after committed-prefix catch-up. Human transitions compare against checked original domain state, never telemetry. Native tests live in `af/tests/it/task_review`; human CLI tests cover lifecycle, original Evidence/Subject/Snapshot provenance and truthful missing-prefix coverage. The generated-scaffold fake-model test in `onboard` proves original producer/model attribution across a separate zero-token human Task. `af-telemetry::it sources::only_af_links_the_reader_module_and_af_process_links_nothing` remains unchanged. |
| Retained stress and preflight tools | `scripts/stress-proof.py` selects current native evidence reuse, waiver across a real head change and actionable empty-Diff refusal alongside its other retained families. `scripts/test-stress-proof.py` checks current Rust test ownership and unknown-name refusal; repetition/load/count requirements are unchanged. `scripts/test-task-preflight.py` retains owned-child and dynamic-budget advisories without the retired graph field or inert `or None` placeholder. |

Store publication is typed and checks leases, committed sequence, CAS identity, exact prefixes and
source ancestry. Native Review initializes the Ledger with `Ledger::for_task_subject`; identity
policy remains `report-derived@1`. Generic Store appends cannot manufacture authoritative Review.
The current Store tests retain canonical identity, corruption refusal, report scope and convergence.

Native onboarding emits the common catalog, current Worker/Pipeline packages and an explicit Review
Task file. Doctor validates the same bounded captured authority. Proposal export reads current-format
Task history and checks original claims, source bases, sealed manifests and committed heads. The TUI
lists Review Tasks through common Task history and Findings; it has no Campaign pane/backend.

## Native Worker transport ownership

Production dispatch is `af-runtime/src/task/host.rs` → `af_runner::task::WorkerContract` →
`invoke_model_with_policy` → the registered `ClaudeTaskAdapter`, `CodexTaskAdapter` or
`PiTaskAdapter`. The captured `WorkerRequest` supplies instructions, exact typed input values,
retry feedback and output schemas. Provider registry bindings and adapter constructors enforce
model/effort, capability and security policy before paid dispatch.

The Campaign-only `ResolvedReviewer`, `ReviewerInputs`, `ReviewerInputArtifact`,
`ReviewerAttemptContext`, `ReviewerAdapter`, `CommandAdapter`, `compose_model_prompt` and
`compose_command_input` family is removed. Its flat result parser, tolerant key aliases,
proposal/notes transport declarations, focus composer, package constructors (`ClaudeAdapter`,
`CodexAdapter`, `PiAdapter`), `RenderedInput` and `NativeReplySchema` have no native production
owner. `ReplyContract::ReviewerResult`, its separate flat native schema and the unused
`ReviewerResultContract` selector are removed too. Tests constructing those retired APIs no longer
justify keeping them exported.

`ReplyContract::Request`, native schema projection and strict-reply normalization remain live in
provider probes, allowance dispatch and reasks. `ModelRunner` and `ContextManifest` retain process
capture, cancellation and credential redaction. All native providers already produce
`TaskTokenUsageV3`; the narrow runner `TokenUsage` and its unused widening conversions are retired.
The common usage persister accepts the exact current wide type. U64-boundary, above-u64,
failure/overrun and cold-reopen tests retain exact accounting without a second DTO. Canonical
`review.kernel/ReviewerResult@2`, `ReviewerStageOutput`, Proposal and Notes domain contracts,
`ReportDraftPolicy`/`ReportDraftValidation`, CAS content/artifact identity and generic warm storage
remain live. Native Review Proposal handling lives in `task/review/proposal.rs`; current Notes,
Head Delta and cache selection belong to `task/knowledge.rs` and `task/development/`.

Useful transport controls now exercise typed requests: Claude checks the published Reviewer Result
payload inside the Worker envelope and refuses textual fallback; Codex checks strict normalization,
exact schema-file bytes, immutable raw replies and missing-message charges. The runner's
`prior_bound` tests retain current Subject admission, non-UTF-8 patch bytes, verified CAS spelling,
exact native context delivery and malformed/oversized refusal. Current assignment parity preserves
the 64 KiB prior-Finding bound. Native knowledge/development tests retain same-slot Notes, current
Head Delta, byte bounds, independent private caches and cold/current recovery. Package-only
renderer/focus/prose assertions are retired, while adapter security/model and native Review
assignment/disposition controls remain.

## Current telemetry workload and lifecycle

One `native-task-review@1` benchmark contract replaces historical release tables and pin rewrites.
Before any laboratory claim/build, `executor.telemetry_workload` compares both retained source trees'
current schema resource identities and constraints with the measurement authority using canonical
JSON digests. JSON booleans cannot substitute for numeric constraints on either source. Top-level
descriptive annotations do not establish capability. Equal Cargo.lock release
strings cannot establish capability. The authority already captures the schemas directory.

Before timing, each binary must reproduce every committed package pin with `af catalog digest` and
plan the exact workload as `af/task-inspection@20` with zero Attempts. Invocation planning uses the
same private scripted Provider as its measured runs. Smoke provenance names the canonically
re-pinned package actually planned and executed, not the pristine full-scale digest. A missing/malformed contract, incompatible
binary, changed schema or mismatched package pin refuses with its original diagnostic; no alternate
workload, old receipt rewrite or fallback pin is tried. This allows comparable current base/candidate
measurements while refusing unsupported historical workloads without changing release installation.

The concurrent fixture retains four paced Workers; the dense fixture retains its 50 MiB output and
ordered `CLOCK_MONOTONIC` drain. The Store fixture runs two sequential native Tasks, each with 500 actual checks and four reviewers
reporting 25 minor Findings each: 1,000 checks and 200 Findings total, within unchanged native
node/check bounds. Lossless typed Selects retain every predecessor failure; each final receipt
covers all required named checks. Both finalizations are timed and peak RSS is the maximum of their
`wait4` high-water marks. Smoke retains ten checks total, five per Task, and ten Findings per
reviewer (80 total). The first failed Task stops the sequence with its original diagnostic. Event totals are read from each native Store, never copied from the
retired Campaign event model. All five repositories have canonical package pins.

Current admission and native Task fixtures replace release-table admission and the retired Store
frontend. The runtime smoke checks whether a failed script reached its first Task before reading its stdout; an
earlier exit retains the script's actual stdout/stderr. A local check refused before starting keeps
its canonical `NotRun` result, original `insufficient_disk` reason and failed preflight. It emits no
invented empty runtime evidence and starts no reviewer. Runtime evidence still requires actual spans
or cache observations; that contract and the storage floor remain unchanged.

`af-runtime/src/telemetry/mirror.rs` observes committed `TaskTransition@6` publication, reconstructs
complete canonical native Rounds and catches up the exact committed prefix in sequence order.
Finding dispositions compare canonical states; the unreachable Campaign-event summary branch is
removed. New Report identity, severity and status remain canonical summary inputs. Demand satisfaction and explicit reuse compare exact
Satisfaction/admission identities, seeded from the prior decision Ledger or the discovery Task's
checked prior Demand Set so inherited evidence is
not reported as newly satisfied. Selection uses current Subject or explicit reuse identity, not
final Demand status: attach/satisfy/waive in one native batch retains both canonical records and
emits exactly one satisfaction with the original Evidence ID before the waiver. Cold replay emits
neither again. Prior-history identities suppress historical reuse, while stale evidence without
new reuse admission produces no satisfaction event. Demand summaries retain the Demand's original Subject/Snapshot;
stale and waived events retain their actual committed ordering. Missing recording stays incomplete;
mirror failures cannot alter Store authority. The `review_decisions` tests exercise satisfaction,
compatible/incompatible reuse, real Subject staleness, waiver, disposition and failed recording.

## Retained boundaries

Owner-excluded point 5 remains intact: `AF_VERSION`, project release pins, historical installation
from 0.8.0, dispatch, rollback, installed-version retention, signatures, exact digests and release
provenance. Generic storage inventory, registration, retention, budgets, locks/leases and safe
removal remain, including registered Review Store categories. Current-release replay/recovery is
required. No real-state pruning, release bump, release-lock edit or publication accompanies cleanup.

Five direct dependency edges were removed with their obsolete consumers:

| Removed direct edge | Obsolete consumer |
| --- | --- |
| `af-pipeline-config` → `af-attempt` | Campaign Pipeline budget configuration/validation using the old Review allowance representation. Native Task budget contracts and compilation remain in their current owners. |
| `af-pipeline-config` → `af-check` | Campaign Pipeline Gate/check configuration and conversion. Current captured code-check policy and native check dispatch belong to `af-runtime::task::code`, which still depends on `af-check`. |
| `af-store` → external `toml` | Retired Campaign bootstrap/manifest authority parsing. Current Task Store replay consumes captured typed artifacts, not Campaign TOML. |
| `af-runner-claude` test-only → `af-pipeline-config` | Tests constructing retired Campaign reviewer packages/adapters. Current tests exercise typed native Worker requests. |
| `af-runner-codex` test-only → `af-pipeline-config` | Tests constructing retired Campaign reviewer packages/adapters. Current tests exercise typed native Worker requests and strict reply normalization. |

Removing a direct edge does not mean removing its package from the lockfile. `toml` remains an
external dependency of other current consumers, and `af-pipeline-config`, `af-attempt` and
`af-check` remain workspace crates. Retained external Cargo.lock package versions, sources and
checksums are unchanged. No external dependency is removed, added or upgraded by these repairs.

Product review independence, gates, sandbox authority, budgets, deadlines, accounting and provenance
remain required. The generated performance harness inventory and staged policy use the canonical
`harness-digest --write` tool; shipped package pins use the candidate binary's catalog digest.

## Full and selective gate execution

`make check`, bare `scripts/verify.sh` and `scripts/verify.sh --profile full` use the same full
Make graph. After all full prerequisites succeed, `check` invokes the existing
`review-kernel-container-probes` target. The full check's prerequisites include the ordinary Rust build/test,
scenario Python tests, complete performance Python suite, feature tests, lint and release checks.
The recipe runs after those prerequisites even under parallel Make. A failed prerequisite stops
publication of a successful gate result and prevents the live target from starting.

The target retains its three commands, in order:

1. All seven ignored sandbox container probes in `af-sandbox::container_probes`.
2. The exact ignored timeout-cleanup control in `af-sandbox`.
3. The exact ignored native Task/Review containment regression through the existing runner:

```sh
python3 scripts/nextest-gate.py --repeat 1 --workspace --locked --profile ci \
  --run-ignored only --no-tests fail -E \
  'binary_id(=af-runtime::it) & test(=task_review_execute_checks::a_container_check_executes_on_the_native_task_host)'
```

A missing/unusable runtime, failed sandbox or cleanup probe, or failed native test fails the gate.
An obsolete selector fails with nextest's no-tests status rather than supplying empty coverage.
The native proof executes once, without isolation reruns. Ordinary Rust tests retain their existing
quarantine exclusion and bounded isolation-rerun policy. No deadline, retry bound, classification,
profile, sandbox policy or product authority changes.

Standalone `make test` retains its Rust-only behavior and both `TEST_RUNNER=nextest` and
`TEST_RUNNER=cargo`. It does not invoke the live target. Selective execution still invokes the same
target once when e2e is selected, including FAST and `make check-full`; no second ordinary full
Rust run or second live-target invocation is introduced within either gate route.

`verify.sh`, `gate-owner.py` and `nextest-gate.py` own supervision and writable report storage.
On the full sealed-source route, the ordinary selection writes the gate's `store/ci/junit.xml`; the single native
proof writes `repeat-1/store/ci/junit.xml` in that same external run directory. Its JUnit and exit
status remain separate from the ordinary selection's folded tests record. Writable standalone
checkouts retain normal shared JUnit publication at `target/nextest/ci/junit.xml`. A failing native
proof cannot turn an earlier clean ordinary Rust report into a passing full gate.

## Regression evidence and validation boundary

`scripts/test-nextest-gate.py::check_full_route` executes the actual full wrapper, production Make
graph, ordinary Rust runner and exact ignored native selector against its shared tiny Rust fixture.
Only unrelated suite payloads and the two sandbox probe bodies are controlled stand-ins. Sequential
and parallel success controls prove one ordinary Rust run before all three probe commands; negative
controls separately stop on parallel compilation failure, missing runtime, failed timeout cleanup
and failed native proof. They check exact execution markers, one failed execution without rerun,
external JUnit contents and
unchanged source bytes/modes with no source `target/`. Existing sealed/writable report controls and
the obsolete-selector refusal remain. These are routing proofs, not positive container evidence.
Against the input Makefile, the new full-route control fails after observing only `test-build` and
`test`: no sandbox probe or native test executes.

`scripts/test-test-classes.py::SealedEvidence.test_full_gate_build_precedes_python_tests_with_real_step_receipts`
checks the real wrapper/Make graph and streaming build receipts with controlled suite bodies.
Its command log requires all three exact live commands once for full checks, no live command for
standalone `make test` in either runner mode, and none after failed compilation. The existing class
build/receipt controls retain FAST and selected-class routing. The quarantine regression separately
checks that the ordinary full selection is still filtered while the ignored native proof keeps
its exact binary/test, `--run-ignored only`, `--no-tests fail` and single-run flags.

The routing regression adds six full-wrapper invocations over the existing tiny fixture: 150
ordinary Rust case executions and three ignored native case executions. Nine sandbox/timeout
stand-in commands exercise routing and failure propagation; they launch no containers. There is no
new Rust test, binary, fixture compilation, sleep, timing assertion or scheduling override. The
class-runner suite adds command logging to its existing cases. The real full gate now additionally
executes the already-existing seven live sandbox probes, one timeout control and one native test
(which runs three contained checks); their runtime is measured by the captured full gate, not
estimated from the stand-ins. FAST/selective routes add no product test execution.

The preceding full attempt is cancelled, not accepted: its 3,758 ordinary Rust tests passed, but
its Python suite was interrupted and the native live target was omitted. That run provides no
complete FULL receipt. Earlier focused and FAST results likewise cannot establish this candidate's
full acceptance. Required final evidence is this candidate's own passed captured FULL and
Markdownlint checks, including actual live native containment, followed by independent correctness
and requirements evaluation of the cumulative patch. The captured receipt and policy establish
which profile ran; the product's unchanged FAST code policy does not identify a private Task's
captured check profile. No future check result is claimed here, and no additional future Task is a
substitute for this final Task's required checks.

Local validation of the routing correction uses pinned Rust 1.88.0 and offline dependencies.
The tiny-fixture script selects installed toolchains explicitly while keeping its HOME/XDG roots
private. Focused native runtime tests retain their real source-mutation and missing-runtime
controls. Live-terminal tests and actual live container execution remain required in the native
host gate; the routing stand-ins do not waive them.

| Local command | Result and scope |
| --- | --- |
| `cargo +1.88.0 fmt --all` | Passed; no Rust source changed. |
| `cargo +1.88.0 clippy --workspace --all-targets --locked --offline -- -D warnings` | Passed with no warning allowance. |
| `python3 -I scripts/test-nextest-gate.py` | Passed report/rerun controls, actual sequential/parallel FULL routing, build/probe failure propagation, sealed/writable source and empty-selection controls. Tiny Rust fixture; no container claim. |
| `python3 -I scripts/test-test-classes.py` | 57 passed; full/FAST dispatch, standalone runner modes, build receipts, catalog, selection and quarantine. |
| `cargo +1.88.0 nextest run --workspace --locked --offline --profile ci -E 'binary_id(=af-runtime::it) & test(/^task_review_execute_checks::/)'` | 6 passed; actual native check execution, missing-runtime refusal and source sealing. The ignored live-container test remains for the required host gate. |
| `npx --yes --offline markdownlint-cli2@0.22.1` on the three changed Markdown files | Markdownlint: not run; pinned package unavailable in the npm cache (`ENOTCACHED`). No network installation attempted. |
| `python3 -I scripts/changelog-notes.py --check` | Passed the change-note contract. |
| `python3 -I scripts/test-classes.py audit` | Passed; classification unchanged. |
| `python3 -I scripts/performance/schemas.py` | Passed; generated schema parity unchanged. |
| `python3 -I scripts/performance/cli.py harness-digest --check` | Passed; protected metadata unchanged and no regeneration required. |

The complete chained manifest proof is supplied with the Task requirements and retained with its
execution reports. Reversing only the seven permitted routing entries reconstructs the complete
sealed documentation candidate; reversing exactly its four documentation entries then reconstructs
the complete predecessor. This checks all remaining paths, bytes and executable modes rather than
substituting original main for an intermediate source. Historical ADR bodies, release/storage
contracts, package pins, fixtures and product policy remain outside this correction. Current
`AGENTS.md`/`CONTEXT.md` guidance and native conclusion semantics are preserved.

Current counts below use the documented algorithm and the independently supplied immutable
original-main baseline. Intermediate manifest preservation and cumulative comparison to original
main are separate proofs. This correction changes only the allowed routing, regression and
documentation paths.

## Cumulative physical counts

Baseline: independently supplied tracked Git-blob counts at
`4c65d55d8ce1fe59586c224bf6258580a2c6047f`. Current counts use UTF-8 decoded `splitlines()` and raw
bytes without newline normalization, excluding symlinks, binary data and build/scratch output.
Rust `crates/*/src/**/*.rs` includes inline and dedicated source-tree tests; it is not a count of
production logic alone. This group precedes external Rust tests. Python tests have a `/tests/`
component or a basename starting `test-`/`test_`. Only Rust src plus non-test Python contributes to
combined source lines; schema, fixture, documentation and external-test deletions do not.

<!-- current-contract-counts:start -->
| Category | Original files / lines / raw bytes | Current files / lines / raw bytes | Change files / lines / raw bytes |
| --- | ---: | ---: | ---: |
| Rust src, including inline/dedicated src tests | 501 / 377,365 / 14,896,128 | 461 / 331,924 / 13,086,011 | -40 / -45,441 / -1,810,117 |
| Python source | 124 / 63,845 / 3,393,046 | 124 / 63,859 / 3,392,693 | +0 / +14 / -353 |
| External Rust tests | 308 / 191,267 / 7,381,963 | 261 / 170,644 / 6,616,625 | -47 / -20,623 / -765,338 |
| Python tests | 109 / 65,628 / 3,684,129 | 109 / 66,044 / 3,710,586 | +0 / +416 / +26,457 |
| Root schema JSON | 473 / 108,025 / 2,910,201 | 417 / 92,171 / 2,488,961 | -56 / -15,854 / -421,240 |
| Fixture text | 1,066 / 130,852 / 4,675,825 | 1,096 / 128,431 / 4,664,707 | +30 / -2,421 / -11,118 |
| Documentation text | 580 / 89,851 / 7,868,206 | 583 / 90,195 / 7,909,111 | +3 / +344 / +40,905 |
| Other text | 406 / 51,917 / 2,125,809 | 429 / 52,100 / 2,139,148 | +23 / +183 / +13,339 |

Combined source is **395,783 lines**, versus **441,210** on original main: **45,427
fewer physical lines (10.296004%)**. Rust src includes its inline/dedicated tests.
<!-- current-contract-counts:end -->

Reproduce from the repository root, optionally passing a tracked Git revision in place of the
empty argument list to count immutable blobs without checking them out:

```sh
python3 -I - <<'PYCOUNT'
from collections import defaultdict
from pathlib import Path
import json
import os
import subprocess
import sys

excluded = {'.git', 'target', 'build', 'scratch', '__pycache__', 'node_modules', '.cache'}

def group(p):
    parts = p.parts
    if len(parts) >= 4 and parts[0] == 'crates' and p.suffix == '.rs':
        if parts[2] == 'src':
            return 'rust_src_including_inline_tests'
        if 'tests' in parts[2:-1]:
            return 'rust_external_tests'
    if parts[0] == 'scripts' and p.suffix == '.py':
        test = 'tests' in parts[1:-1] or p.name.startswith(('test-', 'test_'))
        return 'python_tests' if test else 'python_source'
    if parts[0] == 'schemas' and p.suffix == '.json':
        return 'root_schemas'
    if parts[0] == 'fixtures':
        return 'fixtures_text'
    if parts[0] == 'docs':
        return 'docs'
    return 'other_text'

def files():
    if len(sys.argv) > 1:
        entries = subprocess.check_output(['git', 'ls-tree', '-rz', '--full-tree', sys.argv[1]])
        for entry in entries.split(b'\0'):
            if not entry:
                continue
            metadata, name = entry.split(b'\t', 1)
            mode, kind, oid = metadata.split()
            p = Path(name.decode('utf-8'))
            if mode in (b'100644', b'100755') and not excluded.intersection(p.parts):
                assert kind == b'blob'
                yield p, subprocess.check_output(['git', 'cat-file', 'blob', oid])
        return
    for directory, dirs, names in os.walk('.', followlinks=False):
        dirs[:] = sorted(d for d in dirs if d not in excluded
                         and not (Path(directory) / d).is_symlink())
        for name in sorted(names):
            p = Path(directory) / name
            if not p.is_symlink() and p.is_file():
                yield p, p.read_bytes()

totals = defaultdict(lambda: dict(files=0, lines=0, bytes=0))
for path, raw in files():
    try:
        text = raw.decode('utf-8')
    except UnicodeDecodeError:
        continue
    if '\0' in text:
        continue
    row = totals[group(path)]
    row['files'] += 1
    row['lines'] += len(text.splitlines())
    row['bytes'] += len(raw)
print(json.dumps(totals, sort_keys=True, indent=2))
print('combined source lines', sum(totals[k]['lines'] for k in
      ('rust_src_including_inline_tests', 'python_source')))
PYCOUNT
```
