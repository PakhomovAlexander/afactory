# Product backlog

## Worker identity independent of input contract

Status: requested, deferred beyond self-optimizer M1–M3 (2026-09-16).

A single implementer Worker should accept multiple explicitly declared input
contracts, including initial implementation and review-driven repair. Today AF
requires separate implementer and repairer packages because their input contracts
and roles differ, even when they use the same model and principal.

Design one stable Worker identity with typed invocation variants. Keep each
variant's required inputs, output contract, effects, role and resource allowance
explicit and compiler-checked. Preserve author/verifier independence and exact
captured invocation authority. A repair invocation must receive the candidate and
concrete Review findings without requiring a distinct Worker identity.

This should support future Worker caches: key reusable state by Worker identity
plus compatible contract/configuration/source/context identities, with explicit
invalidation and isolation. Stable identity must not imply blindly sharing mutable
sessions, private context, permissions or cached verification results.

Acceptance: one Worker package binds both implementation and repair slots; AF
validates the selected input variant, rejects missing/incompatible inputs, and
records the variant in replay/accounting/cache provenance. Existing single-input
Workers remain compatible. Cache implementation itself can follow separately.

Keep the current separate repair Worker in M1–M3; do not expand these milestones
to implement this redesign.
