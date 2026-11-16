# ADR-2026-11-15-capture-pi-wake-registration-with-traversal-reservation: Capture Pi selection

**Status:** proposed (2026-11-15); candidate repair for issue #934, live certification pending.

## Context

An authenticated Pi poll cached registrations before the route traversal transaction loaded its
own registration set. A concurrent first registration could commit between these reads. The
transaction then reserved the new subscription, but the caller failed to find it in its older
list. The extension treats that subprocess error as terminal and stops automatic polling.

## Considered options

- Retry the failed subprocess in the extension: rejected; leaves inconsistent selection in AF.
- Reload registrations after reservation: rejected; adds another independent view of selection.
- Return the selected registration from the reservation transaction: selected.

## Decision

Return the exact `WakeRegistration` selected inside the immediate Store transaction that validates
the route, reads its complete registration set and persists its route-local traversal position.
The Pi caller consumes that captured registration directly, without a prior registration list or
subscription lookup. Keep all downstream current route, source, expiry, lease, capacity, ordering
and native uncertainty checks. This changes only a Rust helper return type; persisted contracts,
versions, schemas, storage accounting and cleanup are unchanged.

A debug-only observation barrier immediately before selection lets a public CLI regression start
an authenticated poll with no registrations, commit its first registration through `wake register`,
and then continue selection. The poll must return a valid nonterminal response, and fresh polls
must subsequently extract authoritative completion without a foreground drain or manual repair.

This follows the common durable Store and conservative receipt recommendation of accepted research
Document `sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`,
source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`.
There is no deviation from that recommendation or expansion of host authority.

## Consequences

The reserved registration and scheduling position share one serialized view, including registrations
committed after polling begins. Existing publication durability, per-route fairness, backoff,
restart and fail-closed scope regressions remain required. Deterministic fixtures use private
recording hosts only. Independent verification and final Pi/Codex live certification remain
coordinator work; Claude live testing remains deferred.
