# ADR-2026-11-14-refuse-invalid-routes-before-native-wake-admission: Refuse invalid wake routes

**Status:** proposed (2026-11-14); candidate correctness repair for issue #911.

## Context

Native wake ordering traverses the committed outbox across subscriptions. Treating every route
lookup failure as expiry or revocation lets a later accepted packet overtake an earlier packet
whose route is missing, corrupt or substituted. The later native intent permanently fences the
original session, preventing recovery of the earlier packet.

## Considered options

- Skip every failed route lookup: rejected; failure does not prove ineligibility.
- Classify diagnostic strings: rejected; diagnostics are not typed authority.
- Validate retained route authority before returning explicit eligibility: selected.

## Decision

The Store loads and validates the closed route, its exact requested identity, content digest and
Store incarnation before checking expiry or persisted revocation. Its internal eligibility result
returns no route only for validated expired or revoked authority. All parsing, validation, missing
row and database errors propagate through native selection before reservation or admission of a
later packet. Public route lookup continues to refuse expired and revoked grants.

This clarifies the [committed-packet ordering decision](2026-11-13-reserve-native-wake-preflight-and-order-committed-packets.md).
Existing bounded traversal, collection, session fences, retry allowances and receipt stages remain
in force. No persisted or public contract shape changes, migration or compatibility reader is needed.

## Consequences

Invalid earlier authority blocks native progress until repaired; it cannot be silently discarded
to create a later irreversible session fence. Deterministic fixtures cover corruption, missing
rows, substitution, database conversion errors, restart and repair, plus valid expiry/revocation.
Foreign Store authority fails even when self-consistently hashed and expired or revoked. These
tests use local Store fixtures only; Pi/Codex live certification remains separate and Claude live
testing remains deferred.
