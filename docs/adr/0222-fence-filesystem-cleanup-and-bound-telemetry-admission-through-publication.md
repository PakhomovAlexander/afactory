# ADR-0222: Fence filesystem cleanup and bound telemetry admission through publication

**Status:** proposed (2026-10-26).

## Context

A stopped telemetry container does not prove that its candidate-owned execution directory
was removed. Diagnostic telemetry readers also reach the ordinary sample container builder,
so experiment-only restrictions leave writable IPC and daemon logging channels open.
Hashing inputs before starting the experiment deadline leaves admission outside its budget.

## Options considered

- Ignore deletion errors after container removal: rejected because retained filesystem state
  could be reported clean and automatically removed by hosted cleanup.
- Maintain a separate diagnostic container implementation: rejected because readers, ordinary
  samples and seed writers need the same containment policy and native supervisor.
- Check time only after synchronous work: rejected because a blocked read or copy can consume
  the teardown and publication reserve.

## Decision

Telemetry filesystem deletion is checked. Its trusted owner is durably fenced as uncertain
before deletion and settled only after successful removal. Failure retains recovery paths,
blocks further executions and prevents a complete result. Recovery never changes candidate
permissions automatically. Container settlement alone cannot override filesystem uncertainty.

The shared container builder disables IPC and daemon logging. Shared image admission refuses
implicit volumes before samples, seeds or diagnostic readers launch, using their existing
absolute deadline and supervision mechanisms. Diagnostic readers retain the native supervisor.

Both the public telemetry run API and CLI capture entry time before admission. Binary and build
record reads use bounded non-following descriptors and reject special files and replacement.
Authority traversal and hashing, corpus copies, integrity checks, deletion and publication use
that same absolute deadline. Synchronous phases are interrupted using the existing Budget
mechanism. The profile duration and 90-second teardown/result reserve remain unchanged; the
last second of that reserve is kept for withdrawing interrupted terminal files. An interruption
with retained work conservatively fences ownership, and incomplete publication is not admitted.

## Consequences

Safe pre-launch refusals and clean runs remain eligible for hosted scratch cleanup. Uncertain
filesystem state requires explicit operator recovery. Existing owner shapes and golden examples
remain valid; no compatibility reader or statistical rule changes.

`test_telemetry_terminal.py` and `test_telemetry_cleanup.py` cover unprivileged chmod failures,
trusted workflow cleanup and real synchronous interruption. `test_telemetry_diagnostics.py`
traces the reader route. Mandatory hosted measurement smoke additionally exercises that route
with the native supervisor, denied writes, observed daemon policy and declared-volume refusal,
beside the retained eight build and six measurement cases. Source tests do not establish a live
Docker pass, independent review acceptance or rollout readiness.
