# ADR-2026-11-05-limit-structured-worker-attempts-to-one-model-call: One call per structured Worker Attempt

Status: accepted, 2026-11-05.

## Context

A structured Task can declare one Worker Attempt and two total Attempts including Provider
Admission. ADR-0208's corrective reply re-ask can nevertheless send a second Worker request
inside that Attempt after a schema-invalid reply. Its transient Provider recovery can do the
same after a failed call. The Attempt count then understates semantic invocations, even when
token usage correctly sums them. Issue #664 exposed this with an exact singleton source-ID
schema: returning two source IDs caused an undeclared corrective call.

## Considered options

- Relax schemas or accept the second reply as native success: rejected because invalid data
  must fail closed and every semantic retry needs declared authority.
- Special-case a business package or add an environment switch: rejected because the installed
  profile, not business input or ambient configuration, owns execution policy.
- Remove recovery from every Task: rejected because existing Review, Document and report
  behavior is outside this change.
- Add a persisted call-count option: rejected because the captured structured profile already
  supplies the authority and no new configurable or persisted shape is necessary.

## Decision

The captured structured domain selects the internal `SingleCall` model-call policy for Worker
nodes. The shared runner honors it by invoking the adapter once, retaining strict reply/schema
validation and tolerant local JSON extraction, and failing on invalid output without a re-ask.
The common runtime honors the same policy by settling a transient Worker failure without
calling the host again inside that Attempt. Provider identity probes remain token-free checks;
they cannot authorize another semantic invocation.

Further Worker calls require the native scheduler's declared slot and Task Attempt allowances.
Each such Attempt has its own reservation, context, durable retry feedback, raw evidence and
actual reported usage. Wrong provenance remains a domain rejection, never a corrective call.
Unknown usage remains unknown; neither failed calls nor reservations become fabricated charges.
Provider Admission remains a distinct charged operation with its existing recovery policy.
Other profiles retain ADR-0208's re-ask and transient recovery behavior.

This narrows ADR-0208 clauses 1 and 3 for structured Workers only and supplements the structured
profile decision in ADR-2026-11-03-accept-package-defined-structured-tasks. There is no new wire,
artifact, schema or persisted field, so ADR-0145 requires no shape version change.

## Consequences

A malformed, schema-invalid or wrong-provenance output can exhaust a one-Attempt Worker slot.
Operators who want another semantic attempt must explicitly allow it in the Pipeline and Task
limits. Admission plus a single Worker makes exactly two model invocations in the ordinary
successful-admission path, even when that Worker fails. Replaying a finished Task makes none.
Counted runner and native CLI tests cover these cases and preserve ordinary re-ask behavior.
