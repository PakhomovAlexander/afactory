# ADR-2026-11-08-consolidate-current-contracts-and-task-review: Consolidate current contracts and Task review

**Status:** accepted (2026-11-08); current contracts, native Review, telemetry and runner closure implemented.
Cumulative full-profile acceptance remains pending independent final verification; see the
[current implementation and validation record](../design/current-contracts-and-task-review.md).

## Context

Some committed schemas describe documents no current reader or writer admits. Other families
deliberately select different, simultaneously current versions by feature: a higher suffix alone
does not prove obsolescence. Review already shares Task Attempts and accounting, but Campaign
translation still owns domain publication and several capabilities absent from native Task review.
Deleting that translation before porting those capabilities would remove guarantees.

The owner approved the second cleanup's contract, representation, review and telemetry work.
Point 5, removal of historical installation/dispatch/rollback/retention and generic storage
behavior, was explicitly rejected. This approval does not authorize publication or machine changes.

## Decision

1. Remove a schema only after checking current producers, readers, schema references, registries,
   tests and fixtures. Retain a lower version with a concrete live dependency, including current
   readers of immutable originals and schema validation of captured observations. Repair active
   navigation; historical evidence keeps the identity and bytes of what it recorded.
2. The planned end state has one current Task Pipeline format, one CompiledTask format and one
   TaskKind format. Project Briefs, bounded recovery and structured acceptance become capabilities
   of that format rather than reasons to select another version. This concerns Task Pipeline
   formats, not permission to erase Review Pipeline semantics before their port.
3. Consolidate the current Draft, ReviewSubject and SourceOrigin representations while retaining
   their distinctions: document versus source-backed report citations, exact base/candidate and
   Change Set authority, committed versus bound source provenance. Share one representation per
   concept; do not turn absence of provenance or review evidence into a default success.
4. Make Review use one native Task path, retaining useful `af review` planning, rendering,
   execution, inspection, ledger and reporting commands as entrypoints to it. Before retiring
   Campaign translation, port and prove complete required-reviewer Rounds, stable Finding
   identity/history, repair/reopening and convergence, exact accounting, fencing and recovery,
   Proposal Integration/provenance, and slicing/Scatter/whole-Subject Semantic Closure. Shared
   Ledger reducers are reusable; their presence alone does not prove native orchestration parity.
   The [capability plan](../design/current-contracts-and-task-review.md) records actual owners,
   tests and the cutover obligations.
5. A later telemetry stage updates current fixture contracts and expectations. Historical
   telemetry mapping is not repaired in the schema-removal stage. Telemetry remains observational
   and cannot steer execution, acceptance, budgets or recovery (ADR-0153).
6. Every changed persisted or printed shape takes a new version identity, with its writer,
   reader, schema and parity tests changed atomically; the retired version fails closed.
   [ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md) remains
   binding, including clause 4's self-management exception: `af self`, `AF_VERSION` and project
   pins still install and dispatch to pinned releases from 0.8.0 on and verify bytes/signatures.
7. Preserve rollback, installed-version retention, signing/digests, release provenance,
   cross-version installation support, current-release replay/recovery, and generic storage
   management, budgets and pruning behavior (ADR-0144). Perform no pruning as part of this work.
   Only Campaign-specific storage projections may later be replaced as part of the proven native
   Review port. Neither release versions nor release locks change in this stage.

## Considered options

- **Delete every lower-numbered schema.** Rejected: feature-selected current formats, admitted
  executor journal originals and captured Provider observations still need their own schemas.
- **Keep obsolete contracts indefinitely.** Rejected: they obscure current ownership and impose
  validation and maintenance cost without supporting current execution.
- **Replace Campaign review with today's native implementation review immediately.** Rejected:
  common Task execution is already shared, but domain capability parity is not yet established.
- **Fold point 5 into the cleanup.** Rejected explicitly by the owner; dispatch, rollback,
  retention, release integrity and generic storage remain unchanged.
- **Stage removal, atomic contract consolidation, capability port and telemetry work.** Chosen:
  each stage has independently reviewable evidence and preserves useful behavior assertions.

## Consequences

This first stage establishes the ownership baseline, removes only proven obsolete schemas and
repairs performance schema inventory/refusal tests. It changes no live payload shape and implements
none of the later consolidations. The seven live lower-version Pipeline/CompiledTask/TaskKind/
DocumentDraft schemas remain until their respective atomic replacement.

Later stages must update active packages and their canonical catalog pins, schema closures,
fixtures, classified tests and performance harness metadata together when affected. Useful behavior
tests are ported before old orchestration is deleted; only tests exclusively asserting a retired
representation may disappear. Gates, sandbox authority, independent review, budgets, exact usage
and provenance are not relaxed. Task delivery, Scenario publication and Remote Check authority
retain their separate existing rules.

## Implementation record (2026-11-08): current Task formats

The Task-format portion of decision 2 is implemented. It supersedes only the feature-selected
format rules of ADR-0160, ADR-0209 and the structured-Task decision of 2026-11-03; their
capabilities, authority and bounds remain in force. In the current release, every Task Pipeline uses `af.pipeline/5`,
every compiled graph uses `af.compiled-task/4` in `af/CompiledTask@2`, and every installed kind uses
`af.task-kind/3`. Pipeline `/1`–`/4`, compiled graph `/1`–`/3` and kind `/1`–`/2` are retired and
refused. Optional Brief constructors, bounded slot recovery and installed structural acceptance
are capabilities of these formats, with their existing authority and bounds. Structured acceptance
still admits only its data-only operators and cannot become independent semantic verification.

The compiler no longer selects a graph format by its operators or recovery slots. PlanningRequest@1
still exposes only ordinary interfaces, and shared catalogs still grant no Brief constructor
authority; these are explicit capability checks. Nested changed records take new identities too:
`af/task-inspection@18`, `af/task-plan-inspection@4`, `af.catalog-contract-fixtures/2` and the captured
`af.task-engine/2` identity. References to unchanged subshapes follow their current schema owner.
The superseded schemas and their exclusive feature-version assertions are removed; meaningful
Brief, recovery, replay, kind-matching and structural-acceptance assertions use current fixtures.

The earlier consequences describe the stage-1 adoption Snapshot. Draft/ReviewSubject/SourceOrigin,
native Review consolidation and telemetry repair remain later work. This stage does not alter
release dispatch, installation, rollback, retention, signing, provenance or generic storage.
The capability plan records the scoped local validation; final cumulative full-profile verification
remains due only after the later stages, before final patch delivery.

## Current implementation (2026-11-10)

The staged decisions above are implemented: Pipeline `/6`, compiled graph `/6` in
`af/CompiledTask@4`, TaskKind `/3`, Draft `/3`, ReviewSubject `/3` and SourceOrigin `/3` each
have one current representation. Native Tasks own installed Review, continuation, human decisions,
Proposal export, complete Round publication, accounting, repair/Integration and Scatter closure.
Campaign execution translation, bootstrap readers and frontend DTOs are retired; canonical Ledger,
Finding, Demand and `review.kernel/*` domain contracts remain.

Campaign-only reviewer transport, package constructors and the flat native reply schema of
[ADR-0246](0246-carry-the-reviewer-result-schema-natively-on-the-campaign-path.md) are retired.
Native Task requests own captured inputs and output contracts for all three Providers; shared
Reviewer Result, Proposal/Notes domain types, process supervision, capability/security checks,
accounting, warm storage and current recovery remain required.

Telemetry uses one current native fixture contract with type-sensitive exact source-schema and
binary-planner admission. Canonical Satisfaction/reuse identities emit lifecycle telemetry even
when the same decision batch ends with a waiver; prior history and cold replay deduplicate them. Benchmark outputs are `/3` and their registry is `/4`; historical fixture overlays and
release-to-fixture tables are removed. Unsupported workloads refuse without fallback. The
[implementation record](../design/current-contracts-and-task-review.md) names current owners,
retained capabilities, counts and reproducible focused validation. It supersedes the staged
implementation-status prose above, without rewriting the historical adoption decisions.

Point 5 remains excluded. Historical release installation/dispatch, rollback, retention, byte and
signature verification, provenance, generic storage management/budgets and current-release recovery
remain required. No release version/lock change, real pruning or publication is part of this work.
Native fast-gate, correctness and evaluation receipts judge this stage; cumulative full-profile
verification remains a mandatory independent final patch-delivery condition and is not claimed here.
