# ADR-2026-11-16-guard-scenario-http-mutations-after-connection: Guard Scenario HTTP mutations after connection

**Status:** proposed (2026-11-16).

## Context

The generic Scenario kernel authenticates exact signed publication Decisions before dispatch.
The installed GitHub adapter performs further blocking observations and HTTP connections.
Python's HTTP client ordinarily connects lazily inside `request()`. A check performed before
that call cannot establish that the same Decision remains unexpired after DNS/TCP/TLS setup.
This is one transport boundary identified by the implementation lifecycle recovery requirements.
It is independent of introducing startup standing publication authority.

## Considered options

- Trust admission-time expiry alone: rejected because blocking work can outlive the Decision.
- Check expiry immediately before `request()`, retaining lazy connection: rejected because the
  connection still occurs after the check.
- Return a never-dispatched result after any connection-time refusal: rejected because the
  durable journal already records possible dispatch, and recovery must remain conservative.
- Explicitly connect, disable reopening and guard immediately before sending: selected.

## Decision

The HTTP transport serializes the body, disables automatic reopening and explicitly establishes
the connection before invoking its mandatory POST guard. The adapter guard revalidates the exact
request binding and checks any signed Decision's expiry with the real clock. The transport then
checks that its application deadline remains and the socket exists, and binds the socket timeout
to that remaining allowance before sending. GETs also refuse exhausted or unavailable connections.
All connection paths close in `finally`.

The existing durable `dispatched` checkpoint still precedes any possible POST. A guard refusal
after this checkpoint remains uncertain, even when the current process knows it sent nothing.
Reconciliation alone follows; an empty listing cannot authorize another POST. No persisted shape
changes, journal migration, retained signing key, new operation or merge authority is introduced.

This strengthens the timing of the existing exact-decision contract. It does not supersede
ADR-0274's publication authority, Wait or repair clauses. Startup scope, native unsuccessful-Task
repair routing, continuous driving and whole-change independent verification remain separate
unimplemented requirements of issue 834; this decision must not stand in for their design.

## Consequences

Private transport fixtures must implement explicit connection and socket timeout behavior. Tests
cover signed expiry during connection, remaining-time rebinding, missing guards, lost sockets,
and exhaustion before sending, plus uncertain-intent reconciliation without duplicate effects.
They exercise installed production adapter code against mocked HTTPS and local HTTP services.
They do not prove physical reboot, mid-Worker-kill recovery, live GitHub publication or the
complete issue 834 lifecycle. SSH push authorization timing remains outside this change.
