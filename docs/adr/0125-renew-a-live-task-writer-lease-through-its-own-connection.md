# ADR-0125: Renew a live Task writer's lease through its own Store connection

**Status:** accepted (2026-09-29), revised (2026-09-30) for the SQLite write lock. Amends
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

The first version of this record renewed through the second connection only in the last 2 s of
the lease. That test held only the Rust mutex. A Store operation in its append transaction also
holds SQLite's write lock, and a renewal through the second connection must wait for it. No
Store connection set a busy timeout, so every one had rusqlite's implicit 5 s wait. That is
longer than the whole 2 s reserve, so a renewal that met the lock waited past expiry. When the
lock was released, the renewal either failed and cancelled a live writer, or committed a
renewal of a lease that had already expired. The Store checked expiry only when the renewal
started, not when it committed.

## Considered options

- **A longer lease or earlier renewal.** Any fixed margin loses to an operation that runs
  longer, and a longer lease delays every real crash recovery.
- **Letting the same writer renew an expired lease nobody has taken.** This changes what
  expiry means to every successor and reader, and lets lost authority mint a lease.
- **Renewing inside the work's own Store operations.** A renewal in the middle of an operation
  moves the exact sequence prefix that operation already fenced.
- **Always renewing through a second connection.** Every renewal would race the work's
  prefix-fenced appends, which turns routine renewals into spurious conflicts.
- **Waiting for the write lock without a bound.** The heartbeat could no longer decide before
  expiry. A renewal that commits after expiry must be refused anyway, so the wait would only
  turn a clean early cancellation into a late one.

## Decision

The heartbeat opens a second connection to the same Store file when it starts
(`EventStore::reopen`, FULL synchronous, no schema writes, and the busy timeout its caller
names). A Store without a database file, or one that cannot be reopened, keeps the previous
shared-only path.

- **Entry.** Before its work runs, a heartbeat section renews a lease that is already under the
  10 s threshold, through the shared connection. No work can hold the Store yet, so this cannot
  race it. A section therefore never starts close to expiry after an unrenewed gap. If the lease
  cannot be read or renewed here, the first tick fails as before.
- **Observation** of the exact writer, epoch and expiry uses the heartbeat's own connection
  every tick. It stays the read-only operation of ADR-0089. The log is WAL, so observation never
  waits for the work's connection or its write lock. A successor or an expiry is seen while a
  Store operation is still running.
- **Renewal**, when under 10 s remain, still goes through the shared connection. It retries
  that connection every 10 ms, so it stays serialized with the work's own appends.
- **Busy timeout.** The heartbeat's own connection waits at most 1 s for SQLite's write lock,
  then fails with a busy error. One wait is therefore bounded well inside the reserve.
- **Reserve.** Only when 4 s or less of the lease remain and the work still holds the shared
  connection does the heartbeat renew through its own connection. An attempt can fail because it
  timed out on the write lock, or because it lost the sequence race to an append the work
  committed at the same moment. After a failure, a fresh read decides. Expiry, release or a
  successor fails at once. A still-live lease is retried after a pause that starts at 20 ms and
  doubles to at most 200 ms, and the shared connection is tried again first.
- **Floor.** No attempt through the own connection starts with less than 1.5 s left, which is
  the busy timeout plus 0.5 s to commit. When the floor is reached, the heartbeat fails and
  requests cancellation while the writer still holds authority.
- **The Store validates every path alike.** Each renewal runs through `renew_task_lease` with
  its full projection, exact writer, epoch and expiry check and exact sequence fence. The Store
  now also checks the lease's expiry inside the IMMEDIATE write transaction, as it already did
  for owned records. A renewal that waited on any connection for the write lock past its lease's
  expiry is therefore refused, and mints nothing. Lost authority, whether expired, released or
  taken by a successor, mints nothing on any path.

The one-second tick, the 10 s renewal threshold and the 15 s lease are unchanged. A failed
read, a renewal that cannot complete, a poisoned Store lock and a heartbeat unwind still request
cancellation before the owner waits for its work. Ordinary shutdown still does not.

The reserve's size is a trade-off. It must be long enough for the own connection to wait out a
write lock held by a normal Store operation and still commit. From the 4 s mark to the floor, the
heartbeat can wait out a lock held for about 2.5 s into the reserve, and one more lock wait from
there. It must also be short, because a renewal through the own connection can overtake an
append the work is about to make. A version that reserved 5 s, before the entry renewal, fired
in an ordinary CLI Task under the full integration suite. It overtook a Store call that was
about to append, and
`task_heavy::full_s2_review_retains_original_round_and_consumes_independent_fix_receipts` failed
with `Task write lost its sequence/lease comparison`. At 4 s, the reserve is reached only after
the heartbeat has waited 6 s for the shared connection. The entry renewal keeps each section
from starting close to it.

## Consequences

A live writer keeps its lease however long a single Store operation holds the shared
connection, and however slow the machine makes it. It also keeps the lease while that operation
holds SQLite's write lock for up to about 2.5 s into the reserve. Successor fencing and expiry are
also seen sooner, even while the write lock is held. The first reproduction passes, and so does
`a_live_writer_keeps_its_lease_while_the_store_holds_the_sqlite_write_lock_past_the_old_reserve`.
That test holds a real write transaction on the Store file for more than 2 s across the reserve.
`lost_authority_is_fenced_while_the_store_holds_the_sqlite_write_lock` shows that a successor or
an expired lease is still fenced and cancelled under the lock, with nothing appended. Two Store
tests pin the bounded busy wait and the in-transaction expiry check. Before this revision, a
renewal that waited past its lease for the lock was committed.

A write lock held from before the reserve until past the floor still ends the Task. The
heartbeat then cancels it before expiry instead of after. A normal Store operation holds the
lock only for its append transaction, so a hold that long means the Store is not making
progress.

One residual race is accepted. Each Store call reads its own projection just before it
appends. If the reserve renewal lands inside one such call, between its read and its append,
that append loses its exact sequence comparison and fails closed as a conflict. The reserve is
reached only after the heartbeat has waited 6 s for the shared connection. Without this record,
a hold that long reached expiry and ended the Task every time. Calls that only read, which covers projection, replay, CAS verification and
currentness checks, are unaffected. So are calls that start after the renewal under the same
held guard.

The shared connection keeps rusqlite's default wait for the write lock. It is serialized with
the work, so only another process can hold that lock against it. The in-transaction expiry check
still refuses any renewal that commits after expiry.

The Store is still one SQLite database with one writer at a time. The second connection is not
another Task writer: it appends only the same writer's lease renewals, through the same fenced
entry point. The delivery heartbeat's own loop is unchanged; its work holds the Store only for
single short appends.
