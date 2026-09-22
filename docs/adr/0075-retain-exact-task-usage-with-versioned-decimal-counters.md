# 0075 — Retain exact Task usage with versioned decimal counters

Status: accepted. Date: 2026-09-12. Superseded in part by
[ADR-0113](0113-ga-reads-only-what-ga-writes.md): readers for unversioned and numeric-only usage,
migration on write, and the execution-record, usage and inspection version ladders. Every Task
inspection is `af/task-inspection@11`.

## Context

Native adapters can report any unsigned 64-bit counter. Canonical JSON numbers and SQLite
INTEGER columns cannot represent that entire range. Even individually valid reports can sum
above 64 bits. Rejecting an already reported overrun before recording it loses paid usage.
ADR-0068's cumulative-floor semantics must survive those representations and writer recovery.

## Decision

`af/TaskTokenUsage@1` encodes each reported counter as strict unsigned decimal text. Omitted
components remain absent. Signs, leading zeros, whitespace, nulls, numeric JSON values and
values above `u64::MAX` are refused. Existing unversioned numeric usage blobs remain readable.
Canonical JSON's numeric bound is unchanged.

New settlement and usage-observation records use `af/TaskExecutionRecord@2`, with decimal
charges. Other execution records keep version 1. Reading selects the declared version before
validation and retains the original envelope and identity. Version 1 keeps its original numeric
shape and bound. An equivalent new encoding cannot replace or duplicate an existing settlement.
All Store replay, reference closure, Review Round fencing and inspection use the same decoder.

The existing scoped budget stores committed and reserved aggregates in `u128`, while individual
observations, ceilings and reservation amounts remain `u64`. The Task's original `u32` Attempt
limit bounds aggregate cumulative floors below `2^96`. Every original scope receives only the
increase over its observed floor. Settlement and release preserve sibling reservations; only
unstarted work returns Attempt credit. Overruns remain charged and block further effects.
Reservation identity exhaustion refuses before any mutation. No extra accounting ledger is added.

Worker and Provider hosts return typed usage independently of output success. The common
runtime retains the highest reported charge in the durable Attempt sidecar, then publishes a
versioned usage artifact and cumulative observation before output publication. Sidecar failure
stops execution. Recovery raises abandoned work to its reservation or reported floor, whichever
is greater, and retains the reported component counters in a versioned usage artifact.

The additive `usage_v1_json TEXT` column is authoritative when present. Malformed exact data is
refused without falling back. Historical numeric-only tables remain readable without migration;
writing migrates under a transaction. New writes cannot lower or erase known usage. Decimal
counters never pass through SQLite numeric affinity.

Task inspection becomes `af/task-inspection@3`; the `af/task-list@2` listing gives common
entries the `af/task-list-entry@2` schema. Aggregate charges are decimal strings and execution history shows the
original wire payload and artifact type. Frozen legacy Review reports retain numeric contracts
and explicitly refuse an aggregate they cannot represent; they never truncate recorded spend.

## Alternatives

Clamping provider reports would forgive paid work. Floating-point storage would change exact
usage and replay. Enlarging every legacy JSON number would silently change released contracts.
Separate wide-overrun accounting would give one Task two competing totals.

## Verification

Mixed-version Store tests retain a historical charge of seven and a subsequent `u64::MAX`
report, including reopen and writer-loss recovery. Budget tests preserve an unstarted sibling,
all original scopes, duplicate/lower observations and the exact total `18446744073709551622`.
Worker and Provider failure fixtures retain the wide optional component and charge. Strict
schema tests and malformed-sidecar tests reject lossy representations. Final workspace, CLI
and full-gate results are recorded separately in the implementation checkpoint log.
