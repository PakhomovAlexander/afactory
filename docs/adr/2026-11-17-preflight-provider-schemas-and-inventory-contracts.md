# ADR-2026-11-17-preflight-provider-schemas-and-inventory-contracts: Preflight provider schemas and inventory contracts

Status: proposed. Date: 2026-11-17.

## Context

Issue #1106 reached Codex with a const-only discriminator in the shipped custom triage output.
The strict projector retained the constant but supplied no type. The provider rejected the
request with `invalid_json_schema`; repeating the same schema could not correct it.
ADR-0208 deliberately permits kernel-only output validation when a contract has no strict
projection. That mode must remain distinct from an invalid schema emitted by the projector.

The public schema registry already checks reference closure and serde parity. Shipped Worker
outputs, staged copies and onboarding templates also need an explicit ownership inventory and
whole-request projection checks: one unsupported port disables native projection for the call.
This implements owner steps 1 and 2; it does not accept the earlier inconclusive design Task.

## Considered options

- Add `type` to each duplicated triage schema: rejected because another const-only contract
  would reproduce the defect and package pins would change unnecessarily.
- Retry unconstrained after a schema rejection: rejected because it hides an invalid invocation
  and repeats deterministic failures with paid allowance.
- Replace schema generation or add historical readers: deferred to the separately scoped
  representation and compatibility work, #1117 and #1116.

## Decision

The shared strict projector infers compatible scalar types from `const` and `enum`, retains
explicit type restrictions and null/mixed scalar values, and never traverses literal values as
schemas. Nullable objects/arrays and nested unions retain their structural constraints.
Optional synthetic nulls are removed only when the original contract does not accept them;
normalization preserves undeclared ports for the kernel to refuse.

A pure bounded validator checks the actual emitted dialect. The selected captured Codex Workers
are preflighted before account admission and again against the actual invocation request.
Malformed schemas return a Worker/port/schema-path refusal. Contradictory literal restrictions
are valid unsatisfiable subschemas and select explicit kernel-only projection, preserving
possible alternatives and optional omissions.
Boolean schemas, references, open maps, literal objects/arrays, untyped arrays and other schemas
without a strict projection explicitly retain ADR-0208's kernel-only mode. No invalid emitted
schema silently falls back to that mode. Full payload and semantic validators remain authoritative.

A native `invalid_json_schema` request rejection is an `unsupported_invocation`, using the
existing closed failure representation. Deterministic invocation refusals, like model refusals,
suppress automatic same-input retries. Earlier transient events cannot override a schema
rejection. Native evidence and reported usage survive; absent provider accounting stays unknown.
No persisted shape or version changes, Attempt caps, reservations, security grants or package
bytes change.

`scripts/contract-inventory.json` declares contract families, canonical schema copies,
reader/writer and semantic owners, parity evidence and whole Worker output sets with explained
kernel/negative-fixture exclusions. Offline discovery refuses missing/orphan entries, duplicate
canonical IDs, broken references and unexplained exclusions. Rust tests compile the inventory
and check actual whole-request projection modes. Existing closure, parity and semantic tests
remain independent. The fast and full gates run inventory checks; functional tests include
provider validation, fake native adapter checks and current Store corruption/version refusals.

## Consequences

`make contract-check` is the one-command hermetic contract gate. It reports compilation separately
from warm test execution and makes no wall-clock assertion or performance improvement claim.
The inventory adds maintenance when contracts or package outputs change; changing an exclusion
requires reviewing the entire output set. No live provider, remote check, publication, historical
Store mutation or compatibility reader is part of this gate. See [the contract gate](../contract-gate.md)
for scope, commands and local timing evidence.
