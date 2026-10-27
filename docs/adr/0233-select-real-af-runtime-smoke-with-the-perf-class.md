# ADR-0233: Select real AF runtime smoke with the perf class

**Status:** accepted (2026-10-26); gate PR default selection superseded in part by [ADR-0239](0239-default-every-pull-request-to-fast-validation.md).

## Context

Upstream `8ec6e0a7..62f8ecd0` adds real AF telemetry setup smoke, deterministic serial corpus
preparation without IPC, versioned CLI help oracles and cross-filesystem private AF fixtures.
The bound candidate already contains four validation classes, portability repairs, full Make
phase ordering and measured-boundary interruption fixtures. Both sets of behavior must survive.

## Decision

Integrate the bounded upstream source changes and preserve their tests and documentation.
Register `telemetry-runtime-smoke` as perf beside `telemetry-build-smoke`, dependent on the
same resolver output. Both stable aggregates require its actual success when perf is selected,
and require an intentional skip otherwise. Missing, failed, cancelled and skipped selected
work fail closed. Full, gate and unconfigured release selections include it; fast without
perf explicitly reports NOT RUN. The advisory telemetry producer retains its event policy.

Keep the exact pinned image and actions, credential-free checkout, contained offline release
build, real AF experiment execution, scratch enforcement and original job/step timeouts.
Record build and runtime command costs as separate perf smoke costs, outside class shares.
The regression checks both workflow dependencies and aggregate results for every job state,
including absent results. Corpus preparation and CLI oracle regressions retain upstream scope;
private AF fixtures retain both real same-filesystem linking and forced cross-device copying.

Keep the sealed portability, allocation-refusal and five-operation deadline fixtures unchanged,
including real admission, Budget enforcement, interruption and terminal/lock assertions. Keep
the real Rust build before Python performance tests, native gate ownership and calibration.
Regenerate staged protected hashes with `harness-digest --write`; do not edit `.af/` authority.

## Consequences

Focused source regressions precede sealing. The kernel's full gate and independent reviews
remain acceptance requirements; these tests establish no hosted Docker pass or performance
measurement. There is no publication, budget increase, persisted format or authority change.
