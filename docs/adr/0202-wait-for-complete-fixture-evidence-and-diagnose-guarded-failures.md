# ADR-0202: Wait for complete fixture evidence and diagnose guarded failures

**Status:** proposed (2026-10-23). Applies
[ADR-0124](0124-run-tests-in-parallel-processes-and-link-them-once.md) and
[ADR-0153](0153-record-telemetry-in-its-own-store-and-never-let-it-steer.md).

## Context

The complete native gate passed 3,169 tests and failed four. Three isolated reproductions
passed: project-scope TUI in 2.441 seconds, heavy starter repair in 30.38 seconds and telemetry
Store invariance in 9.024 seconds. In the gate, the TUI observed SETTINGS before the final
statusbar, heavy starter repair took 92.2 seconds and failed with a resource classification at
repair verification, and the telemetry comparison gained a timing-dependent Task transition.

The guarded kernel experiment already runs exclusively within nextest. Its gate failure after
634 seconds had exhausted execution and only baseline output; that does not establish a
deadline cause. A separate isolated-home reproduction passed all eleven public Task receipts
in 837.99 seconds, using pinned Rust 1.88 and Homebrew Python 3.15 rather than the gate's system
Python 3.9. Unrelated guarded tests and release builds were observed concurrently. The original
resource/cadence failure remains unclassified; suite exclusivity is not machine isolation.

## Decision

The project-scope TUI fixture waits for its intended SETTINGS header, every asserted folder and
the final NORMAL statusbar within the same existing wait bound. Retain every folder/footer
assertion and the quit path. A partial PTY frame is not readiness.

Add only heavy starter repair to the existing exclusive realtime nextest override. Like the
heavy-review fixtures, its eleven Attempts retain their exact five-second Worker budgets and
15-second writer lease. No runtime, fixture or Attempt budget changes.

Only the Store-by-telemetry-level comparison passes the existing debug-only
`AF_TEST_CLOCK_FIXED_MS` through its fixture environment injection, matching the established
`telemetry_never_steers` fixture. Freeze each run at its current wall time: a historical epoch
would immediately expire native Provider admission, which also checks the real wall clock.
Keep exact event-kind, raw-evidence and accounting equality
assertions. Production real-clock lease/deadline behavior and separate cadence tests stay intact.

Guarded missing-output failures print the exact scenario, nested Task inspection and referenced
invocation, feedback and diagnostic CAS objects, including read errors. Bound the inspection and
each object to 64 KiB of source JSON, retaining explicit head/tail and omitted byte counts above
that size, and at most 64 referenced objects with an omitted count. Follow only those diagnostic
reference keys, never arbitrary input or provenance closure into Worker context. This is
failure-only test output, without a new persisted schema or public receipt format.

## Consequences

The Provider disable implementation, status/sidecar versions, trusted independence knobs and
approved 90-minute whole-check support remain unchanged. Every existing guarded harness check
and assertion remains binding. Focused native checks supply dynamic evidence separately from
source inspection; the kernel's unchanged full gate decides complete acceptance.
