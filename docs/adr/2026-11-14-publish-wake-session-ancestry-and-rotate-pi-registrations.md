# ADR-2026-11-14-publish-wake-session-ancestry-and-rotate-pi-registrations: Preserve wake authority

**Status:** proposed (2026-11-14); candidate repairs for issues #928 and #929.

## Context

SQLite FULL persists machine session bindings but cannot alone publish the Store's directory
ancestry. Losing that ancestry could let another Task Store bypass retained native uncertainty.
Pi polling also visited only the first registration, while native ordering correctly included
committed packets from every registration. An earlier unclaimed packet could therefore block
the attached hook indefinitely after a later registration sorted first.

## Considered options

- Sync only newly created directories: rejected; interrupted attempts leave existing unsynced paths.
- Trust SQLite FULL for ancestry: rejected; row durability does not publish ancestor entries.
- Increase polling bounds or skip an earlier packet: rejected; neither preserves bounded fair order.
- Publish the complete ancestry and reuse durable extraction traversal: selected.

## Decision

Before returning the machine control Store for binding or route use, validate its canonical private
Store and CAS path, then fsync directories from CAS objects through the filesystem root. Open each
directory without following a final link. Retry this complete chain on every opening, including
preexisting directories, and propagate every failure before binding or usable route activation.
The durability grade remains ordinary fsync, matching the existing Store and CAS boundary.

Each authenticated Pi poll reserves one extraction visit from all registrations on its selected
route through the existing transactional traversal cursor. The visit persists before one bounded
source page and at most one transport claim. Caught-up sources and backoff consume visits too.
Native selection still checks all committed packets in their original order; traversal does not
bypass expiry, sender leases, exhaustion, original-session uncertainty or attachment fences.
No persisted shape changes, migrations or additional scheduling allowances are required.

The accepted research Document
`sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`
from source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`
recommends durable original-session authority and conservative ambiguous admission. These repairs
follow that recommendation within the existing common Store receiver.

## Consequences

Deterministic public CLI fixtures fail every directory sync, record the complete successful chain,
model loss of unacknowledged entries and reopen retained bindings against a second Task Store after
native uncertainty. Pi fixtures use reversed registration IDs, an earlier committed packet in
backoff, fresh authenticated poll processes and no foreground drain. Debug-only sync controls
cannot grant authority and are absent from release binaries. No actual power loss or host calls
are performed. Final Pi/Codex live certification remains pending; Claude live tests remain deferred.
