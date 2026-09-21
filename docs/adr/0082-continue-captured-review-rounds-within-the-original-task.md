# ADR-0082: Continue captured Review Rounds within the original Task

Date: 2026-09-12
Status: Proposed; superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): earlier
`TaskTransition` generations.

## Context

Heavy Review may close a Round without reaching convergence, or replace an unfinished Round's
captured input epoch. The common Task runtime must preserve the original resource owner and
canonical Finding/Demand lineage across those changes. Opening another Task or resetting its
budget would lose earlier costs and permit previously exhausted work to start again. Reusing
issue-source refresh would misrepresent a Review authority transition as changed business intent.

## Decision

Use `TaskReviewHandoff@1` to name the exact predecessor and successor Task revisions, plans and
captured Review Round wrappers. A numeric Round step requires the predecessor's canonical closed
report; an input-epoch step requires its exact canonical `RoundInputSuperseded@1` event. Both
remain within the same Campaign, captured policy, contracts, Provider bindings and original
Task limits. The successor revision changes only its revision link and exact new Round inputs.

A protected Store operation validates the canonical lineage and both compiled plans. It refuses
pending common Attempts, expired resources and changed approval authority. Its transaction
compares both Task and Review history prefixes and the successor's current Round fence. The
strict `TaskTransition@2` payload records this handoff; earlier transition generations remain
unchanged. A serialized receipt by itself grants no authority.

The handoff installs the successor graph into the existing Task budget and leaves the new plan
unadmitted. The original absolute deadline, whole-Task Attempt limit, cumulative usage and
retired reservation scopes survive. Numeric Rounds receive their captured Round scopes;
replacement epochs retain the same Round caps. Late usage remains attached to its original
Attempt. Active invocation/output state is cleared, while historical reports and owned-child
registrations remain available for accounting. A generated successor requires a new exact-plan
developer decision before admission and execution.

Round conclusion publication is separate from final Task completion. It retains the actual
canonical verdict and resource status. A heavy conclusion that can continue cannot be assembled
as the final Task result. A light conclusion or terminal heavy conclusion retains its existing
acceptance rules; publication and reopen never repeat paid Worker work.

Read-only inspection@6 exposes the typed handoff receipts and original transition payloads.
`af task explain TASK_ID --plan PLAN_ID` may inspect any plan recorded in that Task, including
a superseded or rejected plan. Its separate `af/task-plan-inspection@1` output distinguishes the
selected historical plan from the current plan. Inspection neither restores approval nor admits
execution, and an unrelated CAS artifact is insufficient to establish Task membership.

## Considered options

- A Task per Round would split the business operation's resource owner and history.
- Issue-source refresh has different provenance and input invariants.
- Treating every non-passing Round as a finished Task would prevent the configured heavy
  convergence pipeline from continuing through its captured authority.
- A typed, checked handoff preserves canonical Review evidence and the common Task lifecycle.

## Consequences

Heavy Review can keep one durable Task across canonical Round and input changes. Validation
must retain historical plan bindings for late accounting while refusing them for current work.
The CLI must use the same protected continuation and admission path when its legacy frontend
moves to the common runtime.

Implementation verification and the requested external specialist review are in progress. This
checkpoint does not by itself complete the legacy frontend cutover or live pilot evidence.

The frozen implementation tree `c81893d92b3d9b8a88aafe2cb792c474fd78c2d3` passed the full
local gate at `cff20f3`: 1,018 tests, zero failures, 15 ignored, formatting, Clippy,
documentation tests and byte-identical fixtures. Product commit `faeb933` has that exact tree.
Specialist PR review remains pending; this checkpoint does not claim CLI execution cutover or
automatic Integration support.
