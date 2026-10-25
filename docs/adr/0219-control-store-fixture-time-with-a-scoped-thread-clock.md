# ADR-0219: Control Store fixture time with a scoped thread clock

**Status:** proposed (2026-10-25).

## Context

The full native gate reported two accounting/provenance fixtures failing at
`start_task_attempt` with `Task reservation is not dispatchable`. Both reserve a
1,000 ms Attempt and do real CAS/SQLite preparation before dispatch. Their shared
writer lease is 1,000,000 ms; it is separate from the Attempt deadline. Context
capture consumes that deadline, as
[ADR-0066](0066-reserve-task-attempts-before-binding-exact-context.md) requires.

Advancing only Store time to the reserved deadline deterministically reproduces
both original dispatch failures: the second wide-usage Attempt and the typed
Review provenance Attempt. Dispatching at deadline minus one millisecond succeeds
with the same allowances. This establishes the fixture race mechanism, without
claiming a measured scheduling delay inside the historical gate's preparation.
No production defect was demonstrated.

## Options considered

- Increase the Attempt or verification allowance: changes the scenario and leaves
  another wall-clock race; rejected.
- Sleep, retry, or serialize tests: machine load still controls the outcome;
  rejected.
- Use the existing debug environment clock: process-wide state would affect
  concurrent unit tests; rejected.
- Inject a production clock abstraction throughout the Store: unnecessary for
  these synchronous unit fixtures; rejected.
- Add a scoped, thread-local unit-test clock at the existing `task::now` boundary:
  selected.

## Decision

Only `cfg(test)` builds of `review-store` compile `task::test_clock` and its branch
in `task::now`. `with_time` gives a synchronous closure a local `Cell<u64>` and
restores the previous thread clock with a private drop guard. Nested scopes restore
the actual previous clock, including updates made while it was suspended. Panic
unwinding restores it too. Child threads do not inherit it and concurrent threads
hold independent clocks. There is no new environment setting or public API, and
non-test debug and release builds retain their existing clock behavior.

The two failing fixtures use this seam around their existing real Store paths and
dispatch at deadline minus one millisecond. All wide accounting, recovery and
17 provenance cases retain their original allowances and assertions. The provenance
cases additionally reopen before selection and check exact reservation, context,
start time and cumulative accounting.

`real_store_dispatch_deadline_boundary_preserves_reopened_accounting` exercises
reservation, actual CAS context capture, binding, reopening and dispatch with the
original 1,000 ms window. It accepts deadline minus one millisecond and refuses
equality and one millisecond later while the writer lease and Task deadline remain
live. Refusal leaves the durable event prefix and accounting unchanged. Explicit
release or settlement then survives reopening with exact credit and timestamps;
started work loses effect authority at expiry but still settles its usage.

The `scoped_clock_restores_after_nesting_and_unwinding` and
`scoped_clocks_are_independent_on_concurrent_threads_and_not_inherited` tests
exercise nesting, suspended-clock updates, normal exit, panic unwinding,
non-inheritance and simultaneous clock changes on three threads.

## Consequences and limits

This is a unit-fixture decision, not a runtime architecture or persisted-contract
change. Production deadline, lease, ownership and accounting enforcement are
unchanged. The seam controls Store policy time only; real CAS and SQLite writes
and validation still run. It does not simulate asynchronous clock propagation,
host clock skew or scheduler performance. Focused Store checks precede the
unchanged enclosing full gate and independent reviews; they do not replace them
or confer acceptance on ADR-0218 or the retained Performance Lab work.
