# ADR-0064: Reuse structural validation with fresh authority checks

Status: accepted for the Task increment implementation; unreleased.

## Context

The common Task runtime repeatedly validates the same captured plan during preparation,
dispatch, settlement and publication. Full structural recompilation at each boundary adds work
without changing the immutable compiler inputs. The eleven-Attempt heavy Review fixture passed
locally but exhausted its unchanged ninety-second Task deadline on CI before final evaluation.

## Decision

A captured validator immutably borrows its compiler and retains at most one structurally
validated Task revision and Execution Plan. Full equality of both is required for reuse. Rust's
borrow prevents registry, binding and compiler-policy mutation during the validator lifetime.
A changed Task or plan always invokes full compilation again.

Every validation still rehashes the authority artifacts read by compilation: Task revision,
compiled graph, engine, policy, root Pipeline, dependency packages and invocation policies.
Artifact integrity is never cached. Store operations continue to check current lease, approval,
revocation, deadline and resource authority; cached structural validity grants no dispatch right.

Within a single Store operation, reuse its freshly checked projection and plan for output
validation. Durable publication still reads a fresh projection after the domain callback and
uses the existing expected-sequence write permit. Idempotent publication has no append, so it
explicitly repeats that fresh read after the callback as well. No projection survives across
public Store operations through this optimization.

## Validation

Compiler regressions replace and remove every referenced authority object, alter Task facts,
limits, bindings and generated origins, then check rejection through the captured validator.
Store regressions corrupt authority during domain validation for first publication and replay;
neither adds events or loses the existing paid Attempt. Original limits, verifier reservations,
acceptance assertions and frozen reproduction fixtures remain unchanged. Local timing is a
command-fixture observation, not a live-model product benchmark; CI remains the release gate.
