# ADR-0102: Account for every reported Claude Task model

Date: 2026-09-15
Status: Proposed; superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): an
adapter invoked without an explicit model restriction (its default-model compatibility, and any
unambiguous entry explaining the top-level summary); the Task adapter now requires `--model`.
The old provider-smoke accounting left as follow-up work is gone with the Provider Operation.

## Context

The Claude native result can report top-level conversation usage and a `modelUsage` breakdown
that also includes internal client inference. The top-level counters can therefore understate
the complete native invocation. Adding both summaries would count overlapping work twice.
A selected model flag also does not prove that the client used no other model internally.

## Decision

The generic Claude Task adapter recognizes a bounded map of at most 32 model entries, with
native u64 counters widened to exact u128 totals. Each model contributes input, output and
cache-creation charge once; cache reads remain informational under the existing convention.
The top-level bill must match either the aggregate or the selected model entry. With no
explicit model restriction, an unambiguous entry can explain the top-level summary. Absent
maps retain the previous top-level-only interpretation and artifact encoding.

Malformed or oversized maps retain known contributions but cannot appear complete. Conflicting
top-level and model summaries overlap: preserve the larger known charge, not their sum.
Repeated canonical identities may also overlap; preserve their maximum observed charge with
unavailable component dimensions. Use existing `TaskUsageObservation@1` incompleteness and
reservation floors. Invalid nonbilling metadata refuses output without inventing unknown billing.

An explicit model selection accepts an exact native map key or an explicit native
`canonicalModel` match; contradictory identities refuse. There is no inferred alias or date
stripping. Foreign entries with all four explicitly zero usage dimensions are unused metadata.
Positive cache reads count as activity even though they do not increase the charge convention.
Unexpected positive or unknown usage refuses successful output while retaining usage and raw
evidence. An adapter invoked without an explicit model restriction retains its previous
default-model compatibility; ordinary Task plans still require explicit model and effort settings.

This check occurs after native execution. It cannot prevent auxiliary inference or its input
delivery. The adapter also owns `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` and
`CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1`. Static native-client 2.1.272 code connects these guards
to the print loop's title-attempt latch before automatic title inference. This supports
suppressing that concrete path, not a universal claim about all internal inference or egress.
Synthetic child tests prove the variables reach the child while preserving USER, HOME and
CLAUDE_CONFIG_DIR grants. No Provider registry changes or alternative credentials are used.
The native `--bare` mode requires API-key authority, so it cannot replace personal OAuth.

These constants are bound by the generic CLI's captured engine identity: a new binary changes
`af.task-engine/1`, and compiler restoration refuses an engine different from the original
RunAuthority. The domain invocation-policy artifact itself is unchanged. Existing captured
Tasks require their old engine; this change does not retrofit its flags into old receipts.

## Considered options

- Charge top-level usage only: loses internally reported inference.
- Add top-level and per-model totals: double-counts their overlapping work.
- Discard malformed maps: erases valid paid contributions.
- Reject every zero foreign entry or require a new model pin: changes existing default-model
  compatibility without evidence of an unauthorized invocation.
- Amend old receipts during replay: changes historical authority and accounting identities.

## Consequences

New generic Task returns use the existing exact usage and observation contracts. Settled
artifacts, their producer/context identities and Store replay remain unchanged. A native bill
may exceed its reservation; recording that overrun never grants another invocation.

Synthetic protocol, process, timeout, CAS-outage and artifact-reopen checks cover this adapter.
Legacy Review envelope and old provider-smoke accounting remain separate follow-up work; this
change does not claim to fix their top-level-only interpretation or enforce a client-wide
single-model egress boundary.
