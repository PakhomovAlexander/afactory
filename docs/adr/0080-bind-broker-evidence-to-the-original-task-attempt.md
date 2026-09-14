# ADR-0080: Bind Broker evidence to the original Task Attempt

Date: 2026-09-12
Status: Accepted

## Context

Exact Broker transport and cumulative Task accounting exist, but the historical Broker
binding expects a separately dispatched Review Attempt. Creating that dispatch beside a
common Task Attempt would duplicate execution authority. A connector response can also arrive
after its Task writer, plan or captured Review Round loses authority; that must withhold the
response without losing paid usage.

## Decision

The Store binds one handle to an already started common Attempt. `TaskBrokerBinding@1` pins
the Task revision, plan, invocation, context, original reservation, writer epoch, qualified
node, typed target and exact operation vector. A Worker target pins its existing slot and
invocation policy; package and Provider identity come from the captured effective binding.
A Provider-admission target pins its separate `TaskProviderProbePolicy@1`, with exact
Model execution, Brokered credential mode, fixed probe protocol and operation bounds. Operation authority must fit the original
reservation. The trusted host rederives it from captured policy; serialized evidence grants
no capability.

`TaskBrokerTransition@1` adds binding and operation evidence to the same Task event sequence.
`TaskBrokerOperation@1` carries the existing exact operation receipt. A protected Store entry
point appends the receipt and raises the common cumulative usage floor in one transaction.
Dense ordinals, routes, quotas and terminal handle state remain checked. A paid response after
authority loss is recorded as revoked under its original binding, including after settlement
or plan replacement. Exact duplicates add no event or charge. No Review dispatch, operation
Attempt or second budget is created.

The compatibility Broker lease retains the captured Campaign, Round and Review node when
present for a Worker. A Provider probe retains the Campaign and Round but uses its own
qualified Provider node. Other Tasks use their Task log identity, genesis event and qualified node. Its Attempt
and writer epoch always identify the same common owner.

The runtime retains the concrete exact Broker through invocation, panic handling and output
processing. It collects cumulative usage before the existing exact sidecar and settlement
path. Local connector configuration must match the captured binding and receives the original
Attempt deadline; the installed connector must enforce it. Credentials remain machine-local.
Adapters explicitly declare credential mode; an unsupported client is refused before invocation.

Broker-bearing Task inspection uses `af/task-inspection@4`, adding typed Broker records and
their positions in common history. Ordinary Task inspection retains @3. Existing wire records
and schemas remain unchanged.

## Consequences

Late paid work stays visible after it loses execution authority. Failed receipt persistence
withholds the response; the retained runtime owner still supplies exact usage to crash recovery.
SQLite constraint errors unrelated to sequence uniqueness retain their actual diagnostics.

Provider admission requires its own captured Broker operation policy and original allowance.
`LegacyReviewTaskPolicy@2` captures explicitly configured probe operations; V1 capture and
read-only recompile keep their existing bytes. Probe policies belong to the exact Task authority
and plan dependency closure. They cannot inherit downstream operations or enlarge admission
allowances. Changing them requires a newly admitted plan; generated plans still require review.
Grouping conservatively preserves exact execution, business policy and probe policy identity.
`TaskProviderContext@2` contains only the fixed readiness request and typed capability;
`TaskProviderAdmission@2` records its separate probe policy. Both use the same original Provider
Attempt, deadline and common accounting. Local probe connector configuration has no synthetic
Worker package. Native Claude/Codex adapters remain `trusted_unsafe`; unsupported Brokered
transport refuses before any ambient invocation.
This binding increment alone does not complete legacy CLI, Scatter or heavy continuation cutover.
