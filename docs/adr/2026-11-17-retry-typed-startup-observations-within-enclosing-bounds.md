# ADR-2026-11-17-retry-typed-startup-observations-within-enclosing-bounds: Retry startup observations

**Status:** proposed (2026-11-17).

## Context

Issue 1073 identifies irreversible failure of a retained implementation Run when a startup actor
or remote-base observation encounters a temporary outage. Issue 1074 identifies cadence admission
that omits enclosing invocation and Scenario bounds, including differently bounded shared children.
Both corrections must retain issue 834's complete startup scope, original deadlines and accounting.

## Considered options

- Retry every preflight error: rejected; unknown errors and corrupt authority must fail closed.
- Probe outside retained reservations: rejected; restarting could bypass cadence and accounting.
- Extend a deadline or ignore tighter ancestors: rejected; it would widen captured authority.
- Follow each invocation path and retry typed observations: selected; no new execution authority.

## Decision

Supersede only the full-preflight-once-per-process clause of the
[continuous driver decision](2026-11-16-drive-scoped-implementation-with-retained-progress.md).
Complete preflight runs before each reserved drive, before paid Task execution or external effects.
A positive typed transport/service failure consumes that reservation and retains a transport wait.
The retained cadence, maximum 512 reservations, original deadlines and expiry remain unchanged.
A transport wait cannot spend publication checkpoint credit to bypass that cadence. A restart
reopens the same Run and reservation. Complete successful preflight is required before retrying
runtime dispatch; no partial preflight supplies authority.

The production adapter reports only positively identified transport/service observations as
retryable. Explicit credential rejection is distinct. Unknown refusals, malformed receipts,
identity/scope drift, TLS integrity failure and resource boundaries remain fail closed. A failed
adapter process does not prove a transport failure. Startup failure receipt 1 carries an exact
scope digest and closed failure kind; the existing successful ready receipt remains unchanged.
Driver 2 adds a required nullable typed preflight wait; its enclosing Store becomes version 16.
Writers, readers, schemas, negative fixtures and release capabilities change together. Historical
Stores and plans are neither rewritten nor migrated; retired shapes are refused.

Schedule admission visits each reachable invocation path, including conditional paths and shared
children. Two polls must fit strictly inside the publication step and every enclosing invocation,
Scenario and Run bound. No deadline is extended or reset.

The generic departures from ADR-0274 remain limited to the explicit implementation capability's
native terminal-outcome repairs, startup-scoped publication and continuous Wait driving, as recorded
in the [active-frame decision](2026-11-17-bound-implementation-driving-to-active-frames.md).
Generic signed publication and native nightly authority remain unchanged. Acceptance and independent
whole-change verification remain mandatory. Human merge remains external; no signing key is retained.
No dependency on the unmerged issue 815 or 805 support candidates is introduced.

## Consequences

Focused native fixture coverage exercises a production preflight failure receipt, driver process
loss and restart, retained cadence, recovery of the same Run, one final PR and cumulative native
Task accounting. Domain tests cover confirmed refusals, immutable expiry and resume exhaustion;
schedule tests cover nested/shared paths and exact boundary controls. Python tests cover the real
HTTP and Git observation classification with controlled IO. These are local fixture claims only.
Physical VM reboot, mid-Worker kill, live paid providers and live publication remain untested.
The kernel's final gate and independent Terra/Astra whole-requirements verification remain later
native verification steps; this implementer does not claim their results.
