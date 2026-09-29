# ADR-0125: Renew a live Task writer's lease through its own Store connection

**Status:** accepted (2026-09-29). Amends
[ADR-0089](0089-interrupt-task-work-when-its-writer-heartbeat-fails.md): the heartbeat no
longer observes and renews only through the connection its work holds.

## Context

On a loaded machine (`make check` on every core plus IDE indexing, load average 10-30), healthy
Tasks died with `af task: event store conflict: Task writer lease is expired or fenced`. This
happened in the TUI and warm-layers campaigns and in kernel tests inside `make check`
([issue #134](https://github.com/PakhomovAlexander/afactory/issues/134)).

The lease lasts 15 s. The heartbeat ticks every second and renews when under 10 s remain.
It read and renewed only through the same `Mutex<&mut EventStore>` its work uses. The Store
refuses renewal of an expired lease, because a successor may take a Task only after expiry. So
once any Store operation held that connection past the lease's remaining time, the heartbeat
could not renew. When the operation ended, it found its own lease expired and fenced a live
writer. The same wait also delayed seeing a real successor or an expired lease until the Store
was free. A load-heavy machine makes projection revalidation, CAS verification and FULL-sync
commits slow enough to cross that margin.

A deterministic test reproduces this. The work holds the Store past its lease, and before this
record the heartbeat failed with exactly that error
(`a_live_writer_keeps_its_lease_while_its_work_holds_the_store_past_the_renewal_margin`).

## Considered options

- **A longer lease or earlier renewal.** Any fixed margin loses to an operation that runs
  longer, and a longer lease delays every real crash recovery.
- **Letting the same writer renew an expired lease nobody has taken.** This changes what
  expiry means to every successor and reader, and lets lost authority mint a lease.
- **Renewing inside the work's own Store operations.** A renewal in the middle of an operation
  moves the exact sequence prefix that operation already fenced.
- **Always renewing through a second connection.** Every renewal would race the work's
  prefix-fenced appends, which turns routine renewals into spurious conflicts.

## Decision

The heartbeat opens a second connection to the same Store file when it starts
(`EventStore::reopen`, FULL synchronous, no schema writes). A Store without a database file,
or one that cannot be reopened, keeps the previous shared-only path.

- **Entry.** Before its work runs, a heartbeat section renews a lease that is already under the
  10 s threshold, through the shared connection. No work can hold the Store yet, so this cannot
  race it. A section therefore never starts close to expiry after an unrenewed gap. If the lease
  cannot be read or renewed here, the first tick fails as before.
- **Observation** of the exact writer, epoch and expiry uses the heartbeat's own connection
  every tick. It stays the read-only operation of ADR-0089, and it no longer waits for the
  work, so a successor or an expiry is seen while a Store operation is still running.
- **Renewal**, when under 10 s remain, still goes through the shared connection. It retries
  that connection every 10 ms, so it stays serialized with the work's own appends.
- **Reserve.** Only when 2 s or less of the lease remain and the work still holds the shared
  connection does the heartbeat renew through its own connection. That renewal can lose the
  sequence race to an append the work commits at the same moment. It is retried, at most three
  times, and only while a fresh read still shows this exact writer live.
- **The Store validates every path alike.** Each renewal runs through `renew_task_lease` with
  its full projection, exact writer, epoch and expiry check and exact sequence fence. Lost
  authority, whether expired, released or taken by a successor, therefore mints nothing on any
  path.

The one-second tick, the 10 s renewal threshold and the 15 s lease are unchanged. A failed
read, a renewal that cannot complete, a poisoned Store lock and a heartbeat unwind still request
cancellation before the owner waits for its work. Ordinary shutdown still does not.

The reserve is deliberately small. A first version reserved 5 s. Under the full integration
suite, the heartbeat routinely waited 0.5 to 5 s for the shared connection, so that version fired
in an ordinary CLI Task. It overtook a Store call that was about to append, and
`task_heavy::full_s2_review_retains_original_round_and_consumes_independent_fix_receipts` failed
with `Task write lost its sequence/lease comparison`. With a 2 s reserve and the entry renewal,
the same suite reached the reserve only in the reproduction test.

## Consequences

A live writer keeps its lease however long a single Store operation holds the shared
connection, and however slow the machine makes it. Successor fencing and expiry are also seen
sooner. The reproduction now passes. Two companion tests show that a successor still fences and
cancels an old writer, and an expired lease is not revived, while the work holds the Store and
the old writer appends nothing. All three failed before this record.

One residual race is accepted. Each Store call reads its own projection just before it
appends. If the reserve renewal lands inside one such call, between its read and its append,
that append loses its exact sequence comparison and fails closed as a conflict. The reserve is
reached only after the heartbeat has waited 8 s for the shared connection. At that point the
previous heartbeat was within 2 s of fencing its own writer, and a hold past expiry ended the
Task every time. Calls that only read, which covers projection, replay, CAS verification and
currentness checks, are unaffected. So are calls that start after the renewal under the same
held guard.

The Store is still one SQLite database with one writer at a time. The second connection is not
another Task writer: it appends only the same writer's lease renewals, through the same fenced
entry point. The delivery heartbeat's own loop is unchanged; its work holds the Store only for
single short appends.
