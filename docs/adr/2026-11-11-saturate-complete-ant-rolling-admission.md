# ADR-2026-11-11-saturate-complete-ant-rolling-admission: Saturate complete rolling admission

Status: proposed
Date: 2026-11-11

## Context

The [native retention contract](2026-11-11-bound-native-ant-retention-and-settlement-capacity.md)
keeps exact decimal accounting but saturates the rolling admission comparison. Native Task
reconciliation can discover overruns after several Runs have been admitted. A retired sum above
u64 maximum followed by any retained charge previously failed checked addition, refusing the tick
without recording its budget skip or durable cursor advancement. Retained-only sums can overflow
in the same way.

## Options considered

- Keep checked addition and refuse ticks until expiry: rejected; over-budget usage requires a
  durable `skipped_budget` decision, not a stalled cursor.
- Saturate stored charges or release overflowing usage: rejected; either loses exact accounting
  or admits spending while already over budget.
- Widen the complete comparison to u128: viable, but unnecessary for the safe-integer budget.
- Saturate every addition in the admission comparison: selected; it preserves the existing
  retention contract with a bounded local repair.

## Decision

Saturate the retained-Run fold and addition of the prospective reservation at u64 maximum.
Every admitted Ant budget is below that maximum, so saturation always remains over budget.
Keep exact native charges, cumulative decimal u128 totals, conservative settled buckets,
unknown reservations and rolling expiry unchanged. This changes no persisted shape or authority.

A deterministic native Store regression reconciles three previously admitted Runs to u64-maximum
charges. It covers both retained-only composition and two retired charges plus a retained Run,
then checks budget skips, durable cursor advancement, replay, exact cumulative totals and normal
admission only after the rolling window expires.

## Consequences

Large aggregate overruns suppress new spending without stalling tick persistence. Stored usage
remains exact and inspectable; admission saturation supplies no evidence of successful work.
