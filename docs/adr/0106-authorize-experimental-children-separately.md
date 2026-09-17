# ADR-0106: Authorize experimental children separately inside one Task

Date: 2026-09-17
Status: Accepted

Extends [ADR-0046](0046-add-versioned-task-contracts-with-exact-plan-approval.md),
[ADR-0056](0056-share-planning-accounting-and-authenticate-generated-plan-decisions.md), and
[ADR-0081](0081-register-owned-review-children-in-the-common-task-runtime.md) for bounded
optimization experiments under the authorized M1–M3 implementation. Their existing approval,
accounting and data-only owned-child rules remain in force.

## Context

An optimizer can produce a baseline/candidate execution closure after its fixed outer Pipeline
has started. The outer plan cannot contain the unknown closure, while ordinary owned children
carry data only and inherit one already captured operator. Treating generated Worker, model,
effort or effect definitions as owned-child data would give model output execution authority.
Starting child Tasks would instead create new budgets and recovery logs.

## Decision

Capture an immutable `ExperimentalSlot@2` in the outer compiled graph. The slot names its policy,
allowed interfaces, packages, Workers, effects, child count, depth, concurrency and allowance.
Its cycle-free `outer_plan_binding_id` is computed from the Task revision, policy and logical slot
before artifact IDs are assigned. The Store then proves that the exact slot artifact is embedded
in the current compiled outer graph and `ExperimentPrepared@1` binds the resulting outer plan ID.
This avoids asking an artifact digest to contain the plan digest that itself contains that artifact.

Preparation persists `ExperimentPrepared@1` and `TaskExecutionRecord@5` under the current writer
epoch, then atomically projects the Task to `needs_plan_review`. It creates no child reservation.
A separate `ExperimentPlanDecision@1` binds the complete prepared closure. The host authenticates
its detached signature with the same captured developer key policy used for generated plans and
rechecks expiry and current key policy at decision and registration.

Registration validates the slot, specification, prepared closure, decision, child execution
plan, current outer plan and remaining original allowance in one Store append. Registered child
nodes are resolved beside the immutable outer graph. They use the common scheduler, invocation,
context, Attempt, settlement and publication paths, with exact per-child allowances and slot
concurrency. The parent folds only Store-proven child evidence. Replay retains selected outputs,
so successful children are not dispatched again.

`TaskOwnedChildSet@1` and `TaskExecutionRecord@4` keep their original data-only meaning. Public
inspection generation 10 exports preparation, decision and registration separately. Earlier
inspection and execution generations remain readable.

## Considered options

- Extend `TaskOwnedChildSet@1` with operators and allowances: rejected because persisted data
  would acquire execution authority and change the meaning of historical artifacts.
- Rewrite the compiled outer graph after preparation: rejected because the approved outer plan
  would no longer identify the graph being executed.
- Run one nested Task per arm: rejected because each child would receive a separate budget,
  deadline, concurrency limit and recovery log.
- Treat initial `--execute` or outer plan confirmation as experiment approval: rejected because
  neither action identifies the generated closure.

## Consequences

Experimental runtimes need an installed compiler/host adapter that produces and revalidates the
complete closure. Missing, rejected, expired, revoked or mismatched authority stops before child
reservation. Waiting consumes the original absolute deadline. Dynamic children remain subject to
the original Task ledger and are visible to native optimization history capture.
