# ADR-2026-11-16-guard-scenario-http-mutations-after-connection: Guard Scenario HTTP mutations after connection

**Status:** proposed (2026-11-16).

## Context

Python HTTP requests connect lazily. DNS/TCP/TLS setup can outlive a signed publication Decision
that was valid when the adapter admitted it. Startup admission does not grant publication authority.

## Considered options

- Rely on admission-time expiry: rejected because blocking connection work can outlive it.
- Check immediately before a lazy request: rejected because connection still follows the guard.
- Refund an already dispatched intent after refusal: rejected because restart cannot prove absence.
- Connect explicitly, disable reopening and guard before sending: selected.

## Decision

Serialize the body, disable implicit connection reopening and explicitly connect before invoking
the mandatory POST guard. Revalidate the exact request binding and signed expiry against the real
clock. Then check the remaining application deadline and socket and rebind its timeout before
sending. GETs also refuse exhausted or missing connections. Close connections in `finally`.

The durable dispatched checkpoint precedes possible POST. A later guard refusal remains uncertain,
even when this process observed no send. Recovery only reconciles; an empty listing grants no retry.
This strengthens existing signed authorization timing without superseding ADR-0274 publication,
Wait or repair authority. It grants no standing scope, signing key or merge permission.

## Consequences

Isolated installed-adapter fixtures cover connection-time signed expiry, lost connections,
remaining-time rebinding, missing guards and deadline exhaustion without sending. Uncertain-intent
recovery checks no duplicate POST. These mocks and local HTTP services do not establish live GitHub,
physical reboot, mid-Worker kill or the full issue 834 lifecycle. SSH timing remains outside scope.
