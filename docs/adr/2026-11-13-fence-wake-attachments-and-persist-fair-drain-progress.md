# ADR-2026-11-13-fence-wake-attachments-and-persist-fair-drain-progress: Bound fair wake progress

**Status:** proposed (2026-11-13); candidate repair, live certification remains pending.

## Context

Recorded findings #887 and #888 on the cumulative wake candidate identify starvation when each
bounded drain starts at the first subscription or route, and Pi callbacks that retain authority
after disconnect or replacement. A cleared interval does not cancel an outstanding subprocess.
The accepted research Document
`sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`
(source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`)
recommends bounded durable driving and preserving uncertain native intent. This repair follows
that recommendation within the existing Store receiver and original-session authority boundaries.

## Considered options

- Increase batch limits or ignore empty visits: rejected; neither guarantees progress across
  bounded invocations, and work must remain accounted.
- Keep rotation in process memory: rejected; separate CLI invocations lose progress.
- Cancel only the Pi timer: rejected; a pending callback can still dispatch or stop a new timer.
- Persist fair traversal hints and fence attachment generations: selected.

## Decision

Add the closed `af/task-wake-traversal@1` record and its schema/parity fixtures without changing
existing persisted contract shapes. Store extraction and native route positions independently in
a single bounded, accounted SQLite row. Validate the admitted Store owner and candidate scope,
then select the sorted successor, wrapping once, in an immediate transaction. Commit the position
before attempting work. Removed or expired targets require no retained target row to find the next
successor. Empty and unavailable visits consume the existing batch allowance. Each extraction visit
handles one existing bounded page; caught-up targets leave that invocation's candidate set.

Traversal is scheduling information only. The source checkpoint/outbox transaction, sender claim,
backoff, original expiry, collection serialization and per-session native uncertainty fences retain
their independent checks. A crash can defer a visit until a later cycle; it grants no resend or
Task execution authority. No scheduler, service, new budget or live host call is introduced.

Each Pi attachment owns a generation and polling state. Disconnect, shutdown and replacement
invalidate the generation. Interval handlers and subprocess callbacks check their captured
generation before changing state or sending. A stale callback leaves any committed native intent
uncertain; reconnect never grants automatic native retry. The installed interactive Pi followUp
API and finite attachment/poll/request limits remain unchanged.

## Consequences

Repeated `drain --max-batches 1` invocations and reopened Stores reach later work even when earlier
subscriptions are caught up or routes have empty inboxes. Concurrent drivers serialize traversal
reservations; existing claims still fence delivery. Scheduling writes add one small bounded row.
Delayed callback fixtures exercise disconnect, shutdown and replacement with success, empty and
error responses. Deterministic CLI fixtures use recording stand-ins, not real hosts. Pi/Codex final
live certification and deferred Claude testing remain outside this candidate repair.
