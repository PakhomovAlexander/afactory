# ADR-2026-11-13-reserve-native-wake-preflight-and-order-committed-packets: Reserve wake preflight

**Status:** proposed (2026-11-13); candidate repair, live certification pending.

## Context

Independent review of the wake delivery candidate found three losses of its intended guarantees.
Pi could discard a claimed packet when its original host became busy during an asynchronous pull.
Codex contacted its endpoint before checking persisted retry eligibility. Native admission ordered
receiver claims, allowing a later claimed packet to overtake an earlier committed outbox packet.

## Considered options

- Drop a Pi packet on a busy transition: rejected; its committed uncertainty prevents recovery.
- Keep retry limits only around native intent: rejected; restarted drivers bypass endpoint backoff.
- Order only receiver rows: rejected; transport claim order is independent of extraction order.
- Retain the supported followUp queue, reserve preflight, and order the outbox: selected.

## Decision

An active Pi attachment may queue its already claimed packet through the supported original-session
`followUp` API if the host becomes busy. Session, interactive mode and attachment generation remain
mandatory. Disconnect, shutdown, replacement or process loss preserves native uncertainty.

Before any Codex endpoint contact, an IMMEDIATE Store transaction reserves the first eligible
session packet. Existing bounded delivery attempts, claim deadline and next-attempt deadline also
own native preflight. The reservation consumes an attempt and persists a lease plus subsequent
backoff before returning. A known preflight failure releases only that exact reservation and
persists backoff or exhaustion. Process loss keeps the consumed attempt and deadlines. A stale
reservation cannot defer or commit native intent. Receipt epochs stay unchanged so independently
evidenced inbox receipts retain their exact identities. Peer authentication still precedes native
intent; possible native submission remains permanently uncertain and is never retried.

Native selection traverses the bounded committed outbox in SQLite insertion order, under the same
transaction. This defines a total order across subscriptions for an original session before
transport claims exist. The earliest live packet blocks later packets until its own admission;
revoked or expired routes and durably closed subscriptions confer no admission authority.
The original-session uncertainty fence continues to prevent every later automatic admission.
No persisted typed shape changes, compatibility readers or migrations are introduced.

## Consequences

The driver performs no endpoint contact during persisted preflight backoff, after exhaustion or
behind a session fence. Crash recovery can delay another preflight through its retained lease and
backoff; it cannot reset its allowance. Delayed Pi callbacks use the supported safe queue without
inventing native admission, turn-start or report proof. Deterministic stand-ins cover these repairs;
final live Pi/Codex certification remains separate and Claude remains unavailable.
