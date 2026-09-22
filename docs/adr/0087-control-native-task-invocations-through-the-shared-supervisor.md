# ADR-0087: Control native Task invocations through the shared supervisor

Date: 2026-09-12
Status: Accepted (2026-09-23); superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): the entry
points without a control and the forwarding that kept their previous behavior. Controlled
invocation is the only native Task path, and an adapter honors or refuses each supplied control.

## Context

Source adapters expose cooperative cancellation, but native Task Workers originally accept only
a timeout. A caller cannot stop an in-flight Provider invocation through that boundary. Parent
signal termination is insufficient because the supervisor owns a separate child process group.
A final message printed before cancellation also cannot establish successful completion.

## Decision

Extend the shared `review-process` captured-output supervisor with an explicit cancellation
flag. Preserve existing entry points as calls without a control. Check before spawning, while
waiting for the child, during stdin completion and during held stdout/stderr drains. Cancellation
kills the owned process group and reaps its direct child. Retain captured prefixes under the
existing shared post-kill drain deadline, including chunks delivered after termination. Do not
restart a separate cleanup grace period for each stream.

Add controlled settled capture to `ModelRunner` and optional controlled invocation to
`WorkerModelAdapter` and the typed Worker helper. An absent control forwards the previous
behavior. An adapter that cannot consume a supplied control refuses before invocation; it must
not silently ignore the caller's request. Native Claude and Codex share their existing command
construction and result parsing between ordinary and controlled invocation. Model, effort,
credential and writable authority do not change.

A cancelled native capture refuses its business message even if that message was already
printed. Raw output and reported usage remain available for capture and accounting, including
when CAS publication fails. Cancellation does not erase a paid observation, reset a reservation
or change the original Task limits. Missing or malformed usage requires its own explicit
accounting-completeness contract; transport cancellation does not invent a reported bill.

## Considered options

- Killing only the parent CLI can leave its separately owned child group running.
- A cancellation check only in the child waiter misses blocked stdin and post-exit held pipes.
- Discarding all cancelled output loses reported usage and failure evidence.
- A shared optional control preserves existing callers and gives substitutable native adapters
  the same bounded process behavior.

## Consequences

Actual synthetic native controls observe a live leader and descendant, cancel after flushed
usage exceeding u64, refuse the apparent result and retain exact usage and raw prefixes.
They verify that the direct child is reaped and descendants cannot execute or hold descriptors.
Linux orphan zombies are dead; their reaping belongs to init. Existing source cancellation,
legacy supervision and ordinary native adapter fixtures remain compatibility checks.

This initial boundary does not itself wire TaskRuntime, heartbeat failure or CLI signals to the
control. Those callers require separate forwarding and accounting checks. Cooperative process
interruption also does not manufacture a terminal domain-level cancelled TaskResult. Full
checkpoint verification and specialist review remain separately recorded evidence.
