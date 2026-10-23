# ADR-0200: Isolate targeted starter repair and retain native exit status

**Status:** proposed (2026-10-23). Extends
[ADR-0199](0199-wait-for-complete-fixture-evidence-and-diagnose-guarded-failures.md) under
[ADR-0124](0124-run-tests-in-parallel-processes-and-link-them-once.md).

## Context

The next complete native gate passed 3,172 tests and failed two, without an overall timeout.
Targeted starter repair took 27.888 seconds and exhausted resources at repair verification;
the exact candidate passed an isolated native reproduction in 19.385 seconds. Like heavy
starter repair, it uses five-second Python Workers and a 15-second heartbeat lease.

The captured-admission accounting and replay fixture reported an absent exit code and empty
stdout and stderr. Its assertion discarded the raw exit status and Unix signal. The original
signal remains unknown; an isolated reproduction passed in 14.437 seconds, and no crash report
establishes a cause.

## Decision

Add targeted starter repair to the existing CI nextest realtime override, beside heavy starter
repair. Hold all suite test threads while it executes, retaining every Attempt, assertion,
Worker wall and lease. This provides suite exclusivity, not machine isolation.

Include the debug exit status, raw Unix status and Unix signal in the captured-admission
fixture's failure diagnostic, using the existing Unix-only integration-test platform contract.
Keep its exact success/failure exit-code assertion and all accounting and replay assertions.
Do not infer a production fault or change runtime behavior from the unclassified original exit.

Preserve the operator-authorized gate command `env TEST_THREADS=4 bash scripts/verify.sh`,
its literal argument provenance and 90-minute allowance. General CI and Makefile concurrency
defaults, every gate target and every internal budget remain unchanged.

## Consequences

Both starter repair fixtures execute under the established realtime scheduling policy. A future
abnormal captured-admission exit retains the platform status needed to classify it. The complete
Provider disable feature and its sidecar/status versions, pin dispatch, independence policy and
admission contracts remain unchanged; full native gate acceptance remains the kernel's job.
