# ADR-2026-11-14-isolate-pi-wake-route-traversal: Isolate Pi wake traversal

**Status:** proposed (2026-11-14); candidate correction for issue #929, live certification pending.

## Context

The preceding [publication and rotation repair](2026-11-14-publish-wake-session-ancestry-and-rotate-pi-registrations.md)
uses the foreground singleton extraction position for route-filtered Pi polls. With subscription
IDs A < B < C, route one owning A,B and route two owning C, alternating polls visit A,C,A,C
indefinitely. An earlier committed B packet in backoff can then strand native delivery for route
one. A competing foreground visit also changes that shared position. The complete machine Store
publication fix for #928 remains required and unchanged.

## Considered options

- Increase polling limits: rejected; alternating scopes still starve B within every allowance.
- Visit every unrelated source or ignore earlier packets: rejected; violates bounded work or order.
- Keep per-route positions in memory: rejected; fresh processes and restarts lose progress.
- Persist independently scoped route positions: selected; fixed candidate sets retain fair visits.

## Decision

Replace only Pi's use of the global extraction hint with the new closed
`af/task-wake-route-traversal@1` record, schema and parity fixtures. Keep the existing global
`af/task-wake-traversal@1` writer and reader for foreground extraction and native traversal.
No existing typed shape changes, compatibility reader or migration is introduced. Unknown versions
and extra fields in the new record fail closed.

Under one immediate Store transaction, revalidate the exact installed, unrevoked, unexpired Pi
route and derive its complete registration set from this Store. Require each matching registration
to bind that route's exact Task identity. Validate retained scope, selected subscription, indexed
columns and route expiry together. Reserve the sorted successor, wrapping once, and commit before
one source page and at most one transport claim. Callers cannot supply a changing candidate subset.
Caught-up and backoff registrations consume visits without resetting another scope's progress.

At most 128 route rows exist, each bounded by the existing record byte limit and counted against
the existing control storage allowance, including indexed identifiers. Each row expires with its
route. Maintenance removes expired, revoked and registration-compacted hints under the existing
transaction; it neither retains CAS references nor changes any delivery or uncertainty evidence.
Source checks, sender leases, retries, session order and sticky native admission fences remain
independently authoritative. The visit position grants no delivery or execution authority.

This follows the durable common Store and conservative ambiguity recommendation in accepted
research Document `sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`,
source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`.
There is no deviation from that recommendation or new host capability claim.

## Consequences

Deterministic public CLI tests alternate two authenticated stand-in sessions with adversarial
registration IDs, retain B's earlier packet in controlled persisted backoff, interleave one bounded
foreground drain and restart processes. After backoff becomes due, attached polling alone must
reach B before later same-session admission; both sessions retain uncertainty without resend.
Scope, version, substitution, expiry, revocation and compaction tests retain fail-closed behavior.
The preceding single-route regression and full machine fsync-chain/cross-Store regression remain.
No actual hosts, credentials, services or publication are involved. Independent final verification
and Pi/Codex live certification remain coordinator work; Claude live acceptance remains deferred.
