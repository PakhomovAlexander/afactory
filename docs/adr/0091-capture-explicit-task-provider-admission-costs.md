# ADR-0091: Capture explicit Task Provider admission costs

Date: 2026-09-13
Status: Accepted (2026-09-23); superseded in part by
[ADR-0113](0113-ga-reads-only-what-ga-writes.md): the V1 catalog and run-authority generation
beside V2. `af.task-catalog/2` is the only catalog and `af.task-run-authority/2` the only run
authority; `provider_admission` is optional, and an omitted cost means the fixed 4,096-token,
45-second default allowance.

## Context

Task-file Provider admission originally reserves 4,096 tokens and 45 seconds. The native
client's complete context can cost more than the short acknowledgement prompt. A Document
calibration admission reported 16,331 input tokens, 10,624 cached input tokens and five output
tokens: 5,712 chargeable tokens. The successful acknowledgement remained recorded, while its
reservation overrun correctly stopped the author before a second Attempt began.

More whole-Task headroom cannot repair an individual Attempt overrun. Treating the installed
reservation as a measured native cost also makes a preparation claim stronger than its evidence.
Without the observed cache discount, the same input and output counts would charge 16,336 tokens.
That arithmetic is a sizing observation, not a bound on future model usage.

## Decision

Add `af.task-catalog/2` with a required `provider_admission` object containing positive finite
`tokens` and `wall_ms`. Both use the existing safe-integer range. The project chooses the cost
explicitly; V2 supplies no default. All distinct admission capabilities in that catalog use
the chosen cost, with the existing one-Attempt limit and required-verifier protection.

Capture this object in `af.task-run-authority/2`, alongside the exact catalog content ID.
Restoration checks equality against those captured bytes and passes the cost to the existing
`TaskPlanCompiler::with_provider_admission` API. It never reads the current checkout or adopts
a new cost from local Provider settings. The compiled graph and Execution Plan already retain
the resulting allowance and reject an edited cost through trusted recompilation.

Keep catalog and captured authority V1 unchanged: the new field is forbidden, serialization
omits it, and restoration uses precisely 4,096 tokens and 45 seconds. Old captured catalogs
are not reinterpreted or rewritten. Starters and legacy adapters continue emitting V1.

An explicit V2 allowance must fit the original Task resources together with mandatory work and
verification reserves. It does not add tokens, Attempts or time to an existing Task. The
TaskBudget, cumulative usage, admission receipt, native request and overrun fence stay unchanged.
A successful capability response can therefore coexist with an exhausted Task, and an overrun
still blocks downstream dispatch even when the overall Task budget has room.

## Considered options

- Raising only the Task total does not clear the individual reservation breach.
- Raising the V1 default would alter historical authority and still leave the cost implicit.
- Skipping probes or reusing another Task's receipt changes admission authority.
- Depending on a warm cache makes execution feasibility depend on unpromised native state.
- Per-provider overrides and a separate budget ledger are unnecessary for this explicit
  project policy; the existing compiler and common ledger already support finite costs.

## Consequences and verification

Catalog V2 has a separate closed schema. V1 serialization controls retain the original bytes;
mixed generations, missing costs, null, zero, overflow and unknown cost fields refuse.
Deterministic native substitutes exercise actual CLI planning, CAS capture, Store execution
and fresh-process replay after the checkout changes. A 5,712-token admission breaches V1 even
with ample total headroom. An explicitly configured 32,768-token admission admits synthetic
5,712 and 16,336 observations and permits the original author/check sequence. Reporting 32,769
still exhausts the Task and retains exact usage. An underfunded total refuses during planning.

The 32,768 value is an explicit test/example policy, not a global default or model guarantee.
Live preparation must record both native overhead uncertainty and its chosen finite reservations.
This change grants no calibration retry, new model call or modification to a stopped Task.
