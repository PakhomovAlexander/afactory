# ADR-0251: Run the Task gate on the fast validation profile

**Status:** accepted (2026-10-27)

## Context

Task gate medians were 9 to 14 minutes until 12 October 2026 and 52 to 58 minutes since the
week of 19 October, across 234 Task receipts. CI's check rose from 16 to 76 minutes on the
same four-core runner. A local full Task gate took about 57 minutes: tests 2,317 seconds,
performance laboratory 521 seconds, performance calibration 188 seconds, test build 108
seconds and lint 94 seconds. The perf class includes an exclusive kernel experiment and
feature-on builds that dominate ordinary Task iteration. ADR-0229 already defines a reviewed
`release:fast` profile with functional, e2e and uix classes.

## Decision

This repository's local Task gate invokes `scripts/verify.sh --profile fast`. The wrapper
resolves `release:fast` in the gate context, writes its plan in the private run directory and
runs the shared `make test-build` step before `make check-classes` with that plan. The build
receipt ends the build deadline before class tests begin. The full check continues in CI gate pull requests,
`make check`, `make check-full` and releases. The perf class is covered by those full checks
and by the separate Performance measurement, never by the local Task gate. The existing
`--test-plan` form remains available and a bare `verify.sh` still runs full `make check`.
The check wall allowance, test threads, remote mapping and individual test assertions do not
change.

## Considered options

- Keep every Task gate full: rejected because perf work consumes most of the local check wall
  and delays ordinary Task feedback.
- Add a second selection mechanism: rejected because ADR-0229's reviewed catalog already
  defines the exact fast selection and its evidence.
- Reduce the check deadline: rejected because it would discard valid slow results under load.

## Relation to ADR-0229

ADR-0229's statement that local `make check` remains full is unchanged. This decision selects
the repository Task gate's own command and leaves ADR-0229's catalog and CI rules intact.

## Consequences

A regression covered only by the perf class reaches a gate pull request or release rather than
this local Task gate. Before opening a pull request for a change to anything the perf class
covers, run `make check-full` by hand. The fast plan and class receipts make omitted perf
validation explicit as NOT RUN.
