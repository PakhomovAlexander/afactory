# ADR-0155: Publish a fixture marker whole or not at all

**Status:** accepted (2026-10-20)

## Context

The identity-recheck tests in `crates/af/src/providers/task/currentness_tests.rs` start a
fixture shell that reports its PID in `$auth/ready`, so the test can prove the status leader was
reaped. The fixture wrote it with `printf '%s' $$ >"$auth/ready"`. The shell opens and truncates
`ready` before `printf` writes, so a 150 ms deadline kill between the two left an empty file.
`identity_rechecks_use_remaining_attempt_deadline_and_prespawn_cancellation` tolerates a missing
marker (the deadline may expire first on a loaded machine) but read the empty one as a PID and
failed with `ParseIntError { kind: Empty }` in one kernel gate (issue #278).

## Considered options

- **Skip the reap assertion when `ready` is empty.** Rejected: an empty file is then
  indistinguishable from any other malformed marker, and a test that reads half-written data as
  "absent" invites the same treatment for the next shape.
- **Widen the 150 ms deadline.** Rejected: the deadline is the test's subject (ADR-0114).
- **Write the PID beside the marker and rename it over `ready` (chosen).** `rename(2)` in one
  directory is atomic, so an interrupted fixture leaves either no `ready` or a complete one.

## Decision

Both identity-recheck fixtures publish their PID through one shared snippet, `PUBLISH_READY`:
`printf` writes `ready.tmp`, then `mv` renames it to `ready`. A published marker that does not
parse as a PID still fails the test, naming its content. The 150 ms deadline, the pre-spawn
cancellation and expired-deadline checks, the elapsed-time bound and the reap assertions are
unchanged; the two "never spawned" checks now also require that no `ready.tmp` exists, and the
deadline test discards a temporary that a kill before the rename left behind.

`interrupted_fixture_publishes_a_complete_pid_marker_or_none` pins the guarantee. A stub `mv`
that never returns holds the snippet between its write and its publication, deterministically:
`ready` must be absent there and after the group is killed, and `ready.tmp` must hold the whole
PID. A bounded sweep of 64 kill points then runs the real snippet and requires, at every one,
`ready` to be absent or to hold exactly the shell's PID. Each kill point SIGKILLs its whole
process group, reaps the shell and checks the marker at once. It does not wait for the group to
vanish: a killed `mv` orphan can stay a zombie that `killpg(.., None)` still finds until init
reaps it, and a SIGKILLed process never runs again, so it cannot write. Each kill point uses its
own temporary directory, so a rename already inside the kernel when the signal lands can only
complete in that run's directory, never in a later one's. The stub's group is killed on drop if
an assertion fails first, and every directory is removed on drop.

## Consequences

- A deadline kill can no longer turn the reap check into a parse failure, and no deadline,
  cancellation, cleanup or isolation assertion was loosened.
- The regression never waits on another process's reaping, so a container whose PID 1 reaps
  orphans late cannot stall it.
- A future fixture that hands the test a value through a file publishes it by rename, never by a
  redirection onto the file the test reads.
