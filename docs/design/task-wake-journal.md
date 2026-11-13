# Task wake journal and delivery

The internal journal is now connected to opt-in registration-before-detach, public owner grants,
a bounded foreground driver and a durable local receiver. See the exact
[operator setup, status and recovery commands](../task-wake.md), and the proposed
[delivery decision](../adr/2026-11-13-drive-opt-in-local-task-wakes.md).
This is candidate implementation, with deterministic stand-in acceptance; final Pi/Codex live
certification remains pending and Claude automatic wake remains unavailable.

Checked extraction retains the established 4096-row pages, source/CAS validation, checkpoint
provenance, exact opening identities, maintenance-lease episode rules and atomic cursor/outbox
transaction. No notification digest participates in CAS reachability. The receiver uses that same
FULL-synchronous SQLite boundary for exact bounded packets, separate receipt stages and native
intent. Sender epochs, finite attempts and backoff persist. Original-session native uncertainty
is sticky across restart, route replacement, expiry and body compaction.

Expired unextracted subscriptions now close durably, including under collection serialization,
so they cannot retain the source indefinitely. A live unextracted subscription still blocks
collection. Materialized notifications disclose historical source unavailability after actual
collection. Bodies compact after the finite replay horizon; bounded uncertain session tombstones
remain, and exhausting their capacity refuses admission rather than authorizing resend.

The accepted research Document is
`sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`,
source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`.
Its common Store/outbox/bridge recommendation and conservative ambiguous-send policy are followed.
The production Pi bridge pulls from the Store instead of maintaining a second JSON journal.
Later host-start/report correlation is intentionally not inferred from void APIs or unrelated turns.
