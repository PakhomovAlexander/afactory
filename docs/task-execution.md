# Task execution

The Task runtime is the common execution path behind `af task` and `af review`. A Task is the
durable business abstraction: it captures exact inputs and authority, compiles a Pipeline into an
execution plan, runs Workers through durable, budgeted Attempts, and records typed acceptance
evidence that survives replay. Review is one Task kind and composes inside implementation under
one Task budget and history. This page indexes the runtime's design decisions, contracts and
walkthroughs; the decisions themselves are
[ADR-0046](adr/0046-add-versioned-task-contracts-with-exact-plan-approval.md) onward.

See [preview and confirmation](task-execution/preview.md) for the compact ASCII view,
expanded tree, Claude/Codex workflow and rc2 automation change.

## Fixed design decisions

- Task is the durable business abstraction; review is a Task kind. Every Pipeline has public
  typed inputs and outputs, and review composes inside implementation under one Task budget and
  history ([ADR-0046](adr/0046-add-versioned-task-contracts-with-exact-plan-approval.md),
  [ADR-0052](adr/0052-capture-local-bindings-and-compose-review-acceptance.md)).
- Shared Pipeline/Worker definitions and locks travel through Git; local bindings are captured
  and cannot weaken mandatory acceptance
  ([ADR-0053](adr/0053-resolve-shared-catalogs-only-during-explicit-sync.md)).
- Every generated plan requires an authorized developer's signed approval before execution. The
  exact plan, Task revision and authority bind that approval; model output never provides it
  ([ADR-0056](adr/0056-share-planning-accounting-and-authenticate-generated-plan-decisions.md)).
- Generated optimization children cross a second exact approval barrier. Preparation records the
  complete baseline/candidate closure and pauses at `needs_plan_review` without reserving child
  work. Inspection generation 10 shows the pending closure and its separate decision. Initial
  `--execute` and outer `--confirm-plan` do not approve it
  ([ADR-0106](adr/0106-authorize-experimental-children-separately.md)).
- Every Attempt is reserved before its context is bound; failed, abandoned and late usage stays
  charged on the same ledger, and a finished Task replays without spending
  ([ADR-0049](adr/0049-run-task-workers-through-shared-durable-attempts.md),
  [ADR-0066](adr/0066-reserve-task-attempts-before-binding-exact-context.md),
  [ADR-0068](adr/0068-retain-inflight-task-usage-in-the-common-budget.md)).
- Result construction determines failed execution before acceptance: passing receipts cannot
  make incomplete work Satisfied, and genuine negative verification remains Unsatisfied with its
  evidence ([ADR-0093](adr/0093-derive-code-task-acceptance-from-execution-and-evidence.md)).
- Targeted repair is a distinct acceptance type from complete Review, and a project must opt
  into it ([ADR-0054](adr/0054-keep-targeted-repair-distinct-from-complete-review.md)).
- Historical Campaigns keep their captured execution path; new Review commands run through the
  common runtime, and missing common Task state refuses instead of falling back
  ([ADR-0084](adr/0084-route-new-review-commands-through-the-common-task.md)).
- Provider admission is a paid, captured node inside the Task's own limits; catalog V2 requires
  an explicit admission cost, and native identity is rechecked before every private send
  ([ADR-0090](adr/0090-recheck-native-task-provider-identity-before-private-invocation.md),
  [ADR-0091](adr/0091-capture-explicit-task-provider-admission-costs.md)).

## Where the contracts live

| Contract | Location |
|---|---|
| Task, Pipeline, plan, result and decision wire contracts | `crates/review-core/src/task/`, [`task-contracts-v1`](../schemas/task-contracts-v1.json), `fixtures/task-contracts/` |
| Task file, catalogs, bindings and developers | [`task-file-v1`](../schemas/task-file-v1.json), [`task-catalog-v1`](../schemas/task-catalog-v1.json), [`task-catalog-v2`](../schemas/task-catalog-v2.json), [`shared-task-catalog-v1`](../schemas/shared-task-catalog-v1.json), [`task-developers-v1`](../schemas/task-developers-v1.json) |
| Inspection and listing | [`task-inspection-v10`](../schemas/task-inspection-v10.json), [`task-list-entry-v2`](../schemas/task-list-entry-v2.json), [`task-plan-inspection-v1`](../schemas/task-plan-inspection-v1.json), [`compiled-task-v1`](../schemas/compiled-task-v1.json) |
| Run diagnostics and delivery | [`task-run-report-v2`](../schemas/task-run-report-v2.json), [`task-diagnostic-v1`](../schemas/task-diagnostic-v1.json), [`task-delivery-record-v1`](../schemas/task-delivery-record-v1.json) |
| Review accounting | [`review-report-v3`](../schemas/review-report-v3.json), [`review-report-v4`](../schemas/review-report-v4.json) |
| Executable credential-free fixtures | `fixtures/task-runtime/` (`pagination`, `review`, `review-v2`, `embedded-review`, `bounded-repair`) |

## Walkthroughs

Start with the Task-file walkthrough, then follow the composition pages in order.

- [Task file](task-execution/task-file.md) — plan, explain, run, show, list and deliver a captured Task; state outside the checkout; exit codes.
- [Model bindings](task-execution/model-bindings.md) — native Claude/Codex Workers, Provider registry labels, token-free identity capture, paid capability admission, usage retention and cancellation.
- [Local bindings](task-execution/local-bindings.md) — per-developer `af.task-bindings/1` files that replace Workers without weakening policy.
- [Selection](task-execution/selection.md) — choosing a captured Pipeline before any Planner call; fallback, ranking and persisted refusal reasons.
- [Generated plans](task-execution/generated-plans.md) — the fixed Planner, bounded compiler repair and exact signed developer approval.
- [Shared catalogs](task-execution/shared-catalogs.md) — explicit Git catalog sync and immutable Task-kind profiles.
- [Export](task-execution/export.md) — portable bundles, static contract fixtures and reuse without another Planner call.
- [Starters](task-execution/starters.md) — `af catalog init` profiles and the builtin definitions with their Attempt bounds.
- [Review Tasks](task-execution/review-task.md) — standalone Review through the common runtime and its exit codes.
- [Embedded Review](task-execution/embedded-review.md) — implementation that requires Review and goal acceptance under one budget.
- [Bounded repair](task-execution/bounded-repair.md) — targeted fixes after Findings, with a distinct acceptance guarantee.
- [Heavy Review](task-execution/heavy-review.md) — carrying repair evidence into a complete second discovery Round.
- [Issues](task-execution/issues.md) — read-only local/Jira requirement capture and explicit revision refresh.
- [Documents](task-execution/document.md) — document Tasks with captured sources, content checks and independent acceptance.
- [Run reports](task-execution/run-reports.md) — scheduler diagnostics and domain publication recovery.
- [Self-optimizer economics](task-execution/self-optimizer.md) — declared history capture, exact project economics and the report-only M1 Pipeline.
- [Review report inspection](task-execution/review-report-inspection.md) — exact current Task accounting beside immutable report snapshots.
- [Review compatibility](task-execution/review-compatibility.md) — the operations extracted from the legacy Review executor and the versioned CLI boundaries.
- [Increment structure](task-execution/pr-sequence.md) — how the packages map to walkthroughs and decisions.

The operator-facing guide for the `implement` Task and local delivery is
[Implementation Tasks with `af task`](tasks.md). Capabilities deliberately left out are listed in
[Non-goals](non-goals.md).
