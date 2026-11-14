# ADR-2026-11-13-drive-opt-in-local-task-wakes: Drive opt-in local Task wakes

**Status:** proposed (2026-11-13); candidate implementation, live certification pending.

## Context

Issue #765's internal journal persisted checked pages but exposed neither user registration nor
native delivery. Its expired subscriptions could indefinitely block source collection. The accepted
research Document `sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`
from source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`
recommends a common durable Store outbox with authenticated original-session bridges and separate
receipts. Coordinator Pi 1.4.0 and Codex 0.167.2 trials establish native primitives, without native
idempotency or authoritative ambiguous-send reconciliation. Claude live testing is deferred.

## Considered options

- Promote the experimental Pi JSON journal: rejected; it is not the common Store durability boundary.
- Start substitute SDK conversations or use resume exit status: rejected; neither proves delivery
  to the original interactive host.
- Retry a native invocation after timeout or void return: rejected; possible admission is not absence.
- Install an automatic daemon: rejected; operator authority selects finite foreground driving.
- Use the private Store as a local durable receiver with a pinned host pull/queue bridge: selected.

## Decision

Expose explicit `af task wake` owner grants, registration, inspection, revocation, finite drain and
Pi pull. `run --detach --wake-route` authenticates and commits registration, checked prefix and
launch intent before spawn; only the parent owns registration and the child receives no route.
Ordinary detach stays unchanged. Launch intent never authorizes recovery dispatch.

Linux private owner capability files bind immutable Store/Task opening, original host/session,
follow-up mode, expiry and installed code identity. The Store retains only capability digests.
Pi pull additionally checks its Linux parent executable and actual extension session/mode/idle
state. Codex pins the selected private Unix socket and verifies its kernel peer UID before
calling only the reviewed existing-session queue argv. A bounded machine control Store under the existing accounted Task Store root binds each
original host/session to one authoritative Store incarnation; another Store cannot bypass its
session fence, and expiry never rebinds it. Hostile same-UID owners remain outside
this boundary, as with the Store incarnation. Other platforms refuse rather than invent provenance.

The FULL-synchronous SQLite receiver retains the packet and exact key/digest before returning
inbox acceptance. Transactional sender leases and epochs fence stale senders, with persisted
finite attempts and backoff. Native invocation first commits one intent and an original-session
fence. Both bridges retain admission uncertainty, with no automatic resend or unsafe overtaking.
No exit-zero or void response establishes native admission, turn start or user-visible reporting.
The fixed message requests current inspection under existing host authority; it conveys no AF
execution, login, continuation, spending or publication authority.

Expiry closes subscriptions durably inside collection's existing serialization. Active extraction
still must reach the exact current prefix before collection; envelopes never pin CAS closure.
Expired bodies compact after twenty-four hours. Uncertain native admission retains bounded compact
session/identity/provenance tombstones rather than permitting a new send. Capacity exhaustion
refuses work. All journal state lives in the already accounted Store database/WAL. Existing
metadata contracts remain unchanged; new closed route, grant and receiver records have independent
versioned schemas and parity fixtures. No older-release reader or migration is added.

## Consequences

Operators have a concrete opt-in workflow and deterministic restart/security fixtures, without a
service installation or real host call during implementation. The Pi hook pulls from native Store
persistence rather than owning another journal. Each hook attachment is finite and explicit.
Unknown native admission can permanently block later automatic wakes for that original session;
this is truthful uncertainty until an independently evidenced reconciliation interface exists.
The user can inspect/report manually without clearing the fence. Native turn/report observation
is not fabricated from possible Pi lifecycle seams. Claude reports unavailable without login.
Final live certification remains separate from this implementation and repository verification.
See [operator documentation](../task-wake.md) for bounds and recovery.
