# ADR-0054: Keep targeted repair distinct from complete Review

Status: accepted for implementation, 2026-09-11.

## Context

P09 continues a finding-bearing embedded Review without resetting the Task or introducing a
second discovery Round in the light strategy. A verified fix on S2 is a narrower guarantee than
a complete Review of S2. The original closed Round and S1 claim provenance remain authoritative.

## Decision

Expand repair as a bounded child Pipeline under the existing scheduler and parent budget.
Protected verification and unconditional paid work must fit each call's declared Attempt limit.
Pass, failure and incomplete paths use the existing typed Select operator.

Require an explicit Task verification profile and captured project permission for targeted
acceptance. `af/RepairAllowedImplementation@1` can record complete Review or targeted fix scope;
`af/ReviewedImplementation@1` remains the stronger public acceptance type. A targeted result
cannot substitute for it during compilation or result validation.

The installed `attest_fixes` handler derives current S0-to-S2 Subject scope, preserves original
Finding identities and S1 views, and attests the sealed S1-to-S2 changed paths. The independent
`fix_verify` Worker must return a current per-Finding decision for every claim. `repair_accept`
revalidates exact context, source, checks, Worker provenance and policy, then publishes typed
per-Finding receipts and a targeted RepairAssessment. Original Review artifacts are immutable.

These Task receipts do not reinterpret historical Review resolution events: the legacy
ChangeAttestation admission path binds its Change Set to the active Review Subject, while this
Task continuation explicitly records the repair transition separately from the S0-to-S2
Subject. Historical stores keep their original reader and continuation semantics.

Repeated validation memoizes only already validated immutable Round/context identities within
a single captured domain instance. Fresh processes reconstruct that evidence from CAS. Memo
sizes are bounded by 64 entries; Worker execution and lease renewal do not hold the memo lock.

## Consequences and evidence

The command fixture completes S0 → S1 → one original finding-bearing Review → S2 → independent
fix verification → explicitly confirmed new worktree. Completed replay performs no new Attempts.
Missing, stale, incomplete and negative decisions retain open obligations. Full-review
requirements, omitted verifiers, insufficient reserves, impossible child bounds and a writer
claiming the fix-verifier role are refused before dispatch.

The common Store still owns publication and exact selected evidence. Domain acceptance follows
Select to its original receipt producers and checks each named obligation independently; an
unrelated passing receipt cannot hide a failed obligation under the same verifier policy.

The full increment still requires heavy history continuation, interruption/replay coverage,
legacy Review entry-point migration and P10–P14. This checkpoint is not release completion.
