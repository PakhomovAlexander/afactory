# Task execution increment

**Status:** implementation started, 2026-09-10. Baseline is kernel `5464b38` (0.8.0).
The accepted hub implementation sequence is P00–P14; kernel contracts, fixtures and progress
are canonical here. The hub design is in its `docs/architecture/task-execution.md` and the
delivery plan in `docs/workstreams/task-execution/implementation-plan.md`.

## Fixed product decisions

Task is the durable business abstraction; review is a Task kind. Every Pipeline has public
typed inputs/outputs, and review must compose inside implementation under one Task budget and
history. Shared Pipeline/Worker definitions and locks travel through Git; local bindings remain
captured and cannot weaken mandatory acceptance. Every generated plan requires an authorized
developer's approval before execution. The exact plan, Task revision and authority bind that
approval; model output never provides it.

The engineering value order remains unchanged. Sharability, batteries included and pluggability
supplement it. Claude calls use only `claude-personal`.

## Progress

The captured Review operation host at `eab8d29` passes the full local gate (**903 tests**, zero
failures, 15 ignored); exact-head CI is run `34693346220`. It executes captured Review on common
Attempts and preserves canonical acceptance and restart recovery. The next accounting slice
adds exact cumulative report receipts and typed reviewer provenance. See
[Review compatibility](task-execution/review-compatibility.md),
[ADR-0077](adr/0077-run-captured-review-operations-under-common-task-attempts.md) and
[ADR-0078](adr/0078-bind-review-conclusions-to-exact-task-accounting.md).

The earlier shared Review operations and Round-fencing checkpoint passes the full gate at `5b1461e`
(internal `9a81fbe`): 839 tests, zero failures, 15 ignored, plus documentation tests and frozen
reproduction. The preceding captured frontend passes CI at `50e9e29`.
The legacy Review CLI still requires its execution cutover; the detailed checkpoint history is
in [the three-PR record](task-execution/pr-sequence.md).

- [x] P00: unchanged-source baseline gate and fixture identities recorded below.
- [ ] P01: contracts, schemas, ADR-0046/0047 and fixtures implemented; one review Round
  completed, Findings corrected and final gate passed locally; main integration remains pending.
- [ ] P02/P03: common Store lifecycle, approvals, legacy links and typed compilation are
  implemented and verified locally; PR integration remains pending.
- [ ] P04–P06: common implementation and Review command execution, Task-file CLI and verified
  local delivery pass real-process fixtures. Fixed command implementation now uses the common
  runtime; legacy Review entry-point cutover and live probes remain.
- [ ] P07–P09: local Worker replacement, Git catalog sync, Task-kind packages, embedded Review
  and bounded repair are implemented. Heavy history continuation passes the full gate.
- [ ] P10: deterministic captured selection and CLI routing pass the full gate; PR integration pending.
- [ ] P11: bounded generation, shared planning accounting and signed developer decisions are
  implemented and pass the full gate; PR integration pending.
- [ ] P12: portable export and contract tests pass the full gate. The complete starter catalog
  passes the full gate, including CLI execution, repair, planning, approval and reuse.
- [ ] P13: document execution and independent acceptance pass the full gate. Read-only issue
  capture and local/Jira source conformance pass the full gate.
  Ticket revision refresh and result-scoped delivery pass the full gate.
  Remaining Provider evidence and Review compatibility are still required.
- [ ] P14: compatibility, benchmark and consumer release.

The owner authorized the complete plan in [three PRs](task-execution/pr-sequence.md).
[Review report inspection](task-execution/review-report-inspection.md) separates exact current
Task accounting from immutable cumulative report snapshots, including Provider and business Attempts.
The [run diagnostics and recovery boundary](task-execution/run-reports.md) preserve failures
before an Attempt starts and retry domain publication without invoking the Worker again.
[Reservation and context binding](adr/0066-reserve-task-attempts-before-binding-exact-context.md)
let adapters render the actual persisted Attempt identity before execution.
The [Review compatibility map](task-execution/review-compatibility.md) records the extracted
operations and remaining legacy entry-point connections.
PR 2's current composition checkpoint is described in [local bindings](task-execution/local-bindings.md)
and [embedded Review](task-execution/embedded-review.md).
The [shared catalog walkthrough](task-execution/shared-catalogs.md) describes explicit Git sync
and immutable Task-kind profiles. The [bounded repair walkthrough](task-execution/bounded-repair.md)
records distinct complete-Review and targeted-fix acceptance guarantees.
The [heavy Review walkthrough](task-execution/heavy-review.md) carries independent repair evidence
into the next discovery Round while preserving original claims and history.
The [selection walkthrough](task-execution/selection.md) explains fallback, automatic ranking and
persisted reasons without Planner calls.
The [generated-plan walkthrough](task-execution/generated-plans.md) describes the fixed Planner,
typed compiler repair, one-budget handoff and exact signed approval before generated execution.
The [export walkthrough](task-execution/export.md) describes portable bundles, static contract
fixtures and second-developer reuse without another Planner call.
The [issue walkthrough](task-execution/issues.md) covers read-only capture, explicit revision refresh and local/Jira bindings.
The [document walkthrough](task-execution/document.md) runs a credential-free release-note Task
with captured sources, protected content checks, independent acceptance and recorded Markdown output.
The [starter walkthrough](task-execution/starters.md) creates software, document, review and repair
definitions plus a reusable Planner from the supported contracts and actual byte pins.
Each PR receives one light Round with Fable 5.1/high on `claude-personal` for correctness
and architecture, Opus 5/xhigh on `claude-personal` for performance, and GPT-5.6-Sol/high on
`codex-personal` for bug bounty. These replace the P01 reviewer selection for future PRs.
Required checks remain Markdownlint and `scripts/verify.sh` (the complete kernel gate).
Policy `452b752` is content-locked using installed `af 0.8.0`; all three requested models
passed bounded Provider preflight (11,377 chargeable tokens, no Gates or reviewer Workers).

PR 1 is in progress. The compiler expands calls, proves branch availability and typed selection,
and checks coverage against final-output lineage. Exact package recompilation rejects edited
graphs, closures and Worker bindings. The common Store persists plans, developer decisions,
invocations, reservations, starts, settlements and selected outputs under fenced writer leases.
Recovery reuses settled successes and retains failed, abandoned and late usage. Legacy read-only
links preserve original history and charge identities without duplicating execution.

Command Workers use the existing scheduler and process runner through `TaskRuntime`. Captured
input/output schemas and exact context manifests govern each Attempt. A document fixture proves
execution, replay and final acceptance. A pagination fixture proves durable candidate capture,
S0-to-S1 lineage and independent checks/evaluation on S1. Failed and unavailable checks feed
typed unsuccessful/inconclusive results without invoking the evaluator. Scheduler guards are
kept outside Worker inputs; runtime caches are kept outside candidate trees.

The Task-file CLI supports planning, running, explanation, inspection and explicit local delivery.
Plans and execution use captured source, packages and checks across process boundaries; editing
live files cannot change a resumed plan. Finished runs do not repeat Attempts. Writer leases
are renewable and explicitly released for immediate plan/run handoff. Delivery uses the existing
checked worktree/recovery implementation with a typed journal in the common Store.

Generic Claude and Codex Task adapters preserve non-review output, raw responses and reported
usage on failure. The captured Worker host checks exact bindings, model/effort and context caps
before dispatch. A deterministic model fixture proves schema-failure retry charges one shared
Task allowance. Retry guidance is a typed failure code tied to its Attempt and contract, not a
copy of the failed response. Native Provider identity capture and charged admission now pass the
Task-file CLI fixtures. Each distinct effective capability runs a fixed internal probe under the
same Task budget. Shared slots reuse it; a failed probe retains usage and dispatches no business
Worker. Token-free planning captures canonical account identity, and account changes on resume
invalidate the recorded binding. See [Model bindings](task-execution/model-bindings.md).

Standalone `af review --file` now executes through the common runtime. Its typed Subject binder
and atomic gather/reducer reuse historical Finding/Demand semantics without a second Store or
allowance. Complete finding-bearing Review Tasks retain `changes_requested` and exit 3; missing
reviewers remain incomplete without authoritative partial sets. Both initial execution and
replay preserve the domain exit. See the [Review Task walkthrough](task-execution/review-task.md).

The heavy continuation checkpoint deterministic gate passed with **783 tests, zero failures and
15 existing opt-in probes ignored**, across 105 suites. Formatting, Clippy and frozen synthetic
reproduction passed.
All eleven implementation/delivery behavior cases now pass through the common Store, with their
original source preserved as a compatibility fixture. Native timeout and storage-failure tests
retain reported usage. The final compatibility gate also covers preserved per-Check process
deadlines inside the aggregate check Attempt. P11 adds the authenticated developer command
boundary and generated execution. Live Provider boundary probes and legacy Review entry-point cutover belong to the remaining
increment. See the
[Task-file walkthrough](task-execution/task-file.md),
[ADR-0048](adr/0048-compile-task-ports-and-fence-developer-plan-decisions.md) and
[ADR-0049](adr/0049-run-task-workers-through-shared-durable-attempts.md).

## P01 contract checkpoint

The additive `review-core::task` contracts cover immutable Task revisions, typed public Pipeline
boundaries, bounded static operator declarations, exact plans and developer decisions, Worker
independence, review history and targeted repair continuation. No new execution is admitted yet;
compiler, Store authority and runtime checks remain P02–P06 work. The implementation branch is
`agent/task-execution`; this checkpoint does not claim integration into main or a release.

The requested three-reviewer light Round completed on `066b392`, using installed `af 0.8.0`
and trusted policy/base `98bb904`. Result: **8 Findings (5 major, 3 minor; one duplicated issue)**
and **1 required identity-test Demand**, `Fail(Exhausted)` at the one-Round cap. All concrete
corrections are implemented locally, including canonical wire ordering, typed round-trip identity,
nonempty result outputs, receipt-derived incomplete precedence, Command-worker independence,
compact negative fixtures, and cached schema validators. The original positive fixture IDs and
frozen review artifacts remain unchanged. See [ADR-0047](adr/0047-preserve-task-wire-identity-and-review-completeness.md).

The [audit and dispositions](task-execution/p01-review.md) include the exact
[Campaign report](task-execution/p01-campaign-report.md), three preliminary Gate failures,
corrections to pre-existing test isolation, and all usage. The report's review wall-clock is
**7m45s** and total chargeable usage is **483,057 tokens**, including **48,969** Provider-admission
tokens across Gate recovery. The original Subject still has eight open Findings and one open
Demand; post-review code/test changes are not current-Subject verification receipts. No follow-up
P01 Campaign ran. The newly authorized PRs receive their own reviews of their complete changes.

Final deterministic gate: **667 tests passed, zero failures, 15 existing opt-in probes ignored**
across 89 suites; formatting, Clippy and synthetic fixture reproduction passed. Markdownlint
checked 96 files with zero errors. Prior candidate gates also passed on read-only archives and
with the exact cleared Gate environment. No model review was repeated after these corrections.

**Current resume:** complete legacy Review entry-point migration and remaining conformance,
then finish P14 evidence. The captured-plan performance correction passes local and Linux CI
gates. Canonical Task-to-Review selection passes the full local gate; in-flight usage accounting
preserves observed broker spend through settlement and recovery, with a complete local gate of
823 passing tests and no failures. Refresh, delivery
and requirements-aware independent acceptance alongside Review pass the local full gate.
Signed developer plan decisions are implemented; external PR review awaits Claude personal login.
Fixed implementation cutover is described in
[ADR-0051](adr/0051-compile-fixed-implementation-tasks-into-the-common-runtime.md).
Continue P00–P14 in the approved three PRs; this internal checkpoint does not reduce that scope.

## P00 baseline evidence

Before changing tracked source, ran `make check` at `5464b38` with the pinned Rust 1.88.0
toolchain and an isolated external target directory. Result: **655 tests passed, 0 failed,
15 ignored** across 88 reported test suites. Ignored tests are the existing opt-in live model
and environment probes; this baseline does not claim those ran. Formatting and Clippy passed,
and `fixtures/synthetic/generate.sh --check` reproduced all fixture bytes exactly.

The exact baseline commit preserves the historical fixture/test corpus. These SHA-256 values
identify the selected compatibility anchors:

| File | SHA-256 |
|---|---|
| `fixtures/synthetic/MANIFEST.tsv` | `ebe802e244b99d05240a7b073c5c2e70f2334282292507b23aa0418637b30dea` |
| `schemas/reviewer-result-v1-conformance.json` | `fe309a0e8304906bce135afca612f466eb895de0b1177b7bc7f1d43ace7ae9cc` |
| `fixtures/compatibility/task-implement-v0.8.0.rs.fixture` (original regression source) | `21da1453f236d8f09d1ab4b15ce41274e8b785d5b260491d26d407a2ccee5cb7` |

Compatibility obligations include legacy review result parsing and ledger replay, Campaign
authority, incomplete/resumable review, existing Task completion/failure/delivery, consumer
policy versions and CLI exit statuses. Later packages must extend this evidence without
rewriting the frozen corpus or treating a generic Task completion as review approval.
