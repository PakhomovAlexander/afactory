# ADR-2026-11-12-keep-within-run-accounting-wide: Keep computed Run accounting exact

Status: proposed
Date: 2026-11-12

## Context

The [rolling admission clarification](2026-11-11-saturate-complete-ant-rolling-admission.md)
handled composition across Runs. Required review found that its checked u64 sum inside a Run
still prevented native reconciliation from persisting a second recovered Task charge of
u64 maximum after a nonzero first settlement. The same computed sum feeds validation,
settlement, reservation admission and retention's rolling buckets.

## Options considered

- Clamp durable usage, drop reservations or tolerate a failed validation: rejected; these lose
  accounting or leave native reconciliation unresolved.
- Saturate the Run sum: rejected; retention needs its exact aggregate, not an admission bound.
- Widen persisted individual charges and introduce new wire versions: unnecessary for this
  composition; every individual recovered charge is already representable.
- Compute Run totals in u128 and narrow only the admission comparison: selected.

## Decision

Compute `Run::tokens` and `Run::accounted_tokens` in checked u128 arithmetic. Compare that exact
sum with widened per-run limits in validation, direct settlement and native reconciliation.
Use the same width for prospective root and containing-frame reservations. Failed and cancelled
Runs retain exact over-budget charges; active Runs still cannot exceed their allowances.

Convert a retained Run's exact total conservatively to u64 maximum only inside rolling admission.
Preserve the existing saturating across-Run and prospective-reservation additions. Retention
continues to store exact cumulative and rolling bucket totals as decimal u128 strings. No stored
field, schema, authority, budget or expiry rule changes; no reader fallback is introduced.

A deterministic native Store test settles one Task for 17 tokens, loses a second response,
reconciles that reservation to u64 maximum, reopens and validates the terminal Run, records a
budget skip with cursor advancement, and retires its body into an exact live bucket. Replay and
normal rolling expiry preserve cumulative usage and producer/intent identities. The earlier
retired-plus-live and retained-only regression remains required coverage.

## Consequences

Within-Run composition no longer prevents a durable budget decision or exact retirement.
Public Rust aggregate accessors return u128; inspection wire records continue to expose the
unchanged exact individual charges and decimal retention summaries. This clarification extends
the earlier comparison repair; its failed required review remains historical evidence.
