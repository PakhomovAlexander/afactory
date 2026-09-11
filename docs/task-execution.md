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

- [x] P00: unchanged-source baseline gate and fixture identities recorded below.
- [ ] P01: contracts, schemas, ADR-0046/0047 and fixtures implemented; one review Round
  completed, Findings corrected and final gate passed locally; main integration remains pending.
- [ ] P02/P03: common Store lifecycle, approvals, legacy links and typed compilation are
  implemented and verified locally; PR integration remains pending.
- [ ] P04–P06: common command execution, Task-file CLI and verified local delivery pass
  real-process fixtures; legacy entry-point cutover, model admission and standalone review remain.
- [ ] P07–P09: shared packages, embedded Review, bounded repair and fix verification.
- [ ] P10–P12: selection, bounded generation, developer approval, export and starters.
- [ ] P13/P14: Jira/document demonstrations, compatibility, benchmark and consumer release.

The owner authorized the complete plan in [three PRs](task-execution/pr-sequence.md).
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
copy of the failed response. Production Provider admission and identity capture remain unfinished.

The complete deterministic gate passed with **722 tests, zero failures and 15 existing opt-in
probes ignored**, across 96 suites. Formatting, Clippy and frozen synthetic reproduction passed.
The 11 legacy implementation/delivery cases also pass unchanged. These checks establish the
Task-file checkpoint, not completion of PR 1. The authenticated developer command boundary,
model Provider admission and legacy entry-point/review cutover remain. See the
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

**Current resume:** extend the common runtime to versioned implementation/review adapters and
model Providers. Model admission and developer authentication must use the same Store guard.
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
| `crates/reviewctl/tests/task_implement.rs` | `21da1453f236d8f09d1ab4b15ce41274e8b785d5b260491d26d407a2ccee5cb7` |

Compatibility obligations include legacy review result parsing and ledger replay, Campaign
authority, incomplete/resumable review, existing Task completion/failure/delivery, consumer
policy versions and CLI exit statuses. Later packages must extend this evidence without
rewriting the frozen corpus or treating a generic Task completion as review approval.
