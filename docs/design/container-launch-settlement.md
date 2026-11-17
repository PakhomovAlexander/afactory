# Container launch admission and settled cleanup

Container create acknowledgement fences daemon creation before any payload start. Gate and Worker
start clients carry the original absolute execution deadline into `af-process`, including duplex
export. After process setup and the platform creation lock, admission checks cancellation and
expiry immediately before each OS spawn attempt. Launch and busy retry time consume that same
allowance; waits never restart it. Expired or cancelled admission returns no process ID or usage.
This is a client launch boundary: scheduling inside an admitted OS spawn or daemon request cannot
provide an atomic daemon-side wall-clock guarantee. Existing post-launch supervision still kills
and reaps the owned process group, preserving output and usage evidence.

Creation settlement and removal retain the existing shared cleanup deadline. A failed removal of
an acknowledged full ID may mean a previous removal succeeded before the owner deleted its CID
journal. One same-context `ps --all --no-trunc --quiet --filter id=FULL_ID` can establish absence
only on successful, unheld, untruncated, empty output. Both removal and this probe are bounded by
the remaining cleanup allowance and the existing five-second probe cap. A daemon failure, listed
object or truncated response retains the journal and writable roots. Pending, malformed and old
intents remain refused before probing. No persisted shape changes in this repair.

Portable regressions cover deadline/cancellation after durable acknowledgement, zero/expired
spawn admission, and retained worker/export/helper journals after removal, including failed,
nonempty and truncated absence evidence. The existing live 250 ms timeout probe remains unchanged;
only the native operator gate can validate it in the real daemon context.
