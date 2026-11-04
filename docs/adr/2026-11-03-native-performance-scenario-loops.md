# ADR-2026-11-03-native-performance-scenario-loops: Native performance Scenario loops

**Status:** accepted (2026-11-03). Partially supersedes ADR-0274's existing-Task-only
orchestration contract, and ADR-0275's journal generation. Their accepted bodies remain
immutable; signed publication, local Task
delivery and Remote Check boundaries remain binding.

## Context

One optimization goal needs research, a small candidate, a native matched comparison,
independent verification and reporting. Ordering alone cannot deliver research to a fresh Task.
A durable loop must reserve its complete cost before dispatch, retain uncertain charges and
wait without holding a model session. The Performance Lab already owns execution, native
reports, immutable originals and the common machine lock and fence.

## Considered options

- Amend ADR-0274's accepted body: rejected because accepted decisions are immutable.
- Run arbitrary benchmark shell commands inside Scenario actions: rejected because this would
  bypass native preflight, source provenance, machine coordination and report verification.
- Treat ordered Tasks or an observation boolean as an artifact handoff: rejected because neither
  proves accepted predecessor data reached the dependent Worker.
- Copy owner-selected JSON into authoritative history: rejected because hashes prove bytes,
  not native provenance or adoption.
- Reuse an uncertain submission or reset spending after a date/revision change: rejected because
  it can duplicate compute or release already committed authority.

## Decision

1. Roadmaps partition optimization work by complete goals, never pipeline stages or days. Their
   immutable revisions retain stable goal identities, receipts and accounting. Atomic admission
   claims one eligible goal and reserves its whole loop against per-run, UTC daily, rolling and
   total limits. Active and unknown charges keep unused reservations across restarts and dates.
2. Fresh Task templates pin native document bytes, committed authority, repository and source
   identities, explicit strategy and limits. Reconciliation derives the same native Task ID.
   Only declared accepted predecessor outputs become native Task input bindings. Production
   performance implementation requires Sol medium, Terra high bugs, Sol high correctness and
   Astra medium evaluation on codex-personal; verification excludes the implementer and retains
   all three judges. Command Workers cannot satisfy these model-role obligations.
3. The candidate consumes the accepted research Document through the optional typed
   `research_report` port, and the final report receives that same artifact. Native acceptance,
   exact result/revision, source provenance and a 32 KiB envelope bound are checked before
   planning. The implementation and report Workers receive this port through their declared
   contracts and input schemas, preserving artifact identity in native context receipts.
4. `experiment_submit_v1` is a separate operation/destination capability naming a pinned installed
   adapter. It durably retains intent and input identity and returns before compute. A separate
   token-free service hands accepted Snapshots to native source materialization and Journal
   submission. Deterministic private Git objects retain original Snapshot provenance without
   changing a checkout, branch or remote. Unsupported or torn captures refuse explicitly.
5. Every local experiment and source capture uses the native common machine lock and fence.
   Lost responses reconcile the original; uncertain claims never relaunch. Definitive refusals
   stay observable and do not starve later report verification. A Wait completes only from a
   native offline-verified report matching exact producer, configuration, request and sources.
   Paired timing determines improvement; negative/inconclusive results remain evidence and
   telemetry remains diagnostic. Polling and report verification run no models.
6. `af scenario evidence-source` resolves explicit artifact IDs through native Task Stores. It
   verifies artifact envelopes, recorded input/source receipts, reconstructed review findings
   and recorded adoption/delivery authority. A correctly hashed arbitrary JSON file grants no
   authority. Rebuildable indexes retain bounded finding substance, native experiment outcomes
   and diagnostic History Capture summaries. Immutable bounded packets carry receipt IDs,
   source/content digests, cutoff, freshness basis, compatibility and counted omissions.
7. Changed Scenario/Ant, Store, adapter request and embedded GitHub journal shapes take new
   versions with current readers, writers, schemas and parity tests; old formats fail closed.
   The example complete loop reserves 32 million tokens against a 23 million Task sum and eight
   elapsed days. UTC daily, rolling and total bounds survive restarts. Smaller packages require
   separately reviewed authority and feasible plans.

## Consequences

The [manual-first runbook](../performance-scenarios.md) and inert examples make the complete loop
reviewable and executable with committed owner authority. Hermetic native planning and fake
Provider/adapter tests prove contracts and recovery, not performance. One real complete manual
candidate loop on Claw remains an operator prerequisite to separate pinned daily Ant enablement.
Workers install nothing and run no paid benchmark. Ant enablement grants no publication;
publication still needs its exact signed human Decision and separate destination authority.
