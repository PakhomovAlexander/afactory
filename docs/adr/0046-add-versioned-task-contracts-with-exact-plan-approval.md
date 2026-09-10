# ADR-0046 — Add versioned Task contracts with exact-plan approval

**Status:** accepted for the Task execution increment, 2026-09-10.

## Context

Implementation currently has a separate coordinator and SQLite history; review owns the graph
runtime and common Store. Making Task central requires one execution contract without changing
the meaning of existing Campaigns, Findings, Source Snapshots or review exit statuses. Every
Pipeline, including an embedded Review Pipeline, needs a public typed boundary. The owner
requires developer review of every newly generated plan before execution.

## Decision

Add `af/TaskRevision@1`, `af/TaskResult@1`, `af/Pipeline@1`, `af/ExecutionPlan@1`,
`af/PlanDecision@1`, `af/ReviewHistory@1`, `af/VerificationContinuation@1` and
`af/RepairAssessment@1`. Their Rust payload contracts live in `review-core::task`; the
language-neutral shapes live in the corresponding schemas. Preserve the existing canonical
JSON and content/artifact digest domains. Do not rename `review.kernel/*`, mutate old payload
versions or reinterpret old configuration under new defaults.

A Task revision names normalized typed input bindings, required outputs, acceptance obligations,
source-adapter provenance, exact trusted policy, effect/destination constraints, strategy and
resource limits. A port binds one artifact or an ordered list of unique artifacts, with explicit
type and Snapshot identity. Names and selectors never substitute for content identities.
Verification reserves protect Attempt counts as well as tokens and time. A new revision retains
its exact predecessor; immutable history is the authority for whether that predecessor is valid.

Pipeline definitions use the independently versioned `af.pipeline/1` discriminator. Public ports
declare type, cardinality, optionality and Snapshot affinity. Public evidence outputs declare the
obligations they cover and bind internal producers. Root applicability applies only to selection
at a Task boundary; an embedded call checks the child's interface. Empty Review History is the
only declared root constructor in this version. Admission must verify that both the Task-kind
adapter and trusted policy permit it and that no recorded lineage is being replaced. Calls bind
required inputs explicitly and cannot apply root defaults.

An Execution Plan pins the Task revision, engine, root and transitive dependencies, compiled
graph, normalized inputs, effective Worker packages/settings and invocation policy, acceptance
producers, authority and budgets. Command Workers are explicit and require no invented model
or Provider identity. Model Workers capture their admitted canonical principal. Independence
requires distinct effective package digests, isolated fresh sessions and role-specific context;
the default also requires distinct authenticated principals. Only trusted policy can relax that
last constraint. Unknown identities and aliases cannot establish independence.

The compiler derives generated origins from the verified dependency closure, including nested
calls. The Store rechecks that provenance; deleting a marker cannot make a generated plan trusted.
Any generated definition in that closure puts execution in `waiting(needs_plan_review)`. A
developer decision refers to the exact Task revision, complete plan identity and policy revision.
The decision's actor string is a record, not authentication. Only the trusted developer entry
point may authorize the decision. Approval and admission are separate transactions; an approval
receipt does not dispatch work. Changed inputs, bindings, dependencies, authority or limits
change the plan identity and require another approval. Resume retains an approval only for the
same immutable plan, subject to revocation, deadline and legal lifecycle checks.

Execution, acceptance and the domain conclusion remain separate. A completed review with
changes requested can satisfy a Task that requested a complete review, while retaining review
exit 3. Missing required output always retains review exit 4 and cannot satisfy acceptance.
Usage and operational errors retain their existing exit 2 and 1 behavior. Targeted repair
assessment records exact current Subject/Snapshot, current Gates and each original claim view;
it cannot claim a full review of the repaired Snapshot. Store admission must verify the receipts,
their authority and current affinity before resolving a Finding.

## Validation boundaries and migration

The schemas validate wire shape; Rust validation additionally checks local cross-field
invariants that JSON Schema cannot express, such as reference sets and reserve totals.
Compilation must prove operator signatures, types, lineage, coverage, bounded control and
complete terminal paths. Store admission must resolve exact CAS references and authenticate
authority, spending, leases and transitions. Deserializing a public struct proves none of those
later conditions. No Task artifact is executable merely because its local validation passes.

The P01 contract package does not change CLI dispatch or admit new-format executions. P02 adds
common Store lifecycle and approval enforcement; P03 adds the compiler; P04–P06 move new
implementation and review executions to the shared runtime after parity checks. Historical
review and `tasks.sqlite` histories retain their bytes, IDs, source Store identity and compatible
continuation path. Linking is idempotent and cannot duplicate spending. General Task sealing
must not reuse `SourceSnapshot@1::Capture::Derived`, whose existing meaning requires a validated
review Integration; a separate versioned lineage contract will describe that operation.

## Alternatives rejected

- Renaming old review contracts would change historical identity without solving runtime reuse.
- A second Task executor or dual writes would duplicate authority and spending semantics.
- A CLI-only approval prompt would leave resume and embedded-call dispatch unprotected.
- Untyped public outputs would prevent callers from proving acceptance coverage before spend.
- Binding approval to a Pipeline name would let later file or Worker changes inherit approval.

## Evidence

The frozen baseline is recorded in [the implementation status](../task-execution.md).
`fixtures/task-contracts/v1/` holds positive and forbidden payloads plus canonical content IDs.
Schema/type parity tests distinguish shape checks from semantic checks. These contract fixtures
are not runtime, migration, authentication or approval-enforcement evidence; later packages must
provide those proofs before new-format admission is enabled.
