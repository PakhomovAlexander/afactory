# ADR-0231: Run Python performance tests after the real build

**Status:** accepted (2026-10-26)

## Context

The full native gate ran `scripts/test-performance.py` as preflight before Rust compilation.
The supplied Task receipt ended at the unchanged 2,400,000 ms build deadline while the suite
was still running, with no build receipt. Progress markers are not a completed suite. The
existing 5,400,000 ms test allowance starts only after the real build finishes (ADR-0207).

## Decision

Move the laboratory suite from preflight into a mandatory `perf-python-tests` target in full
`make check`, recorded by `ci-step.py` as `stage=tests`. Depend on the existing Rust `test`
target so the successful `cargo test --locked --no-run` and its genuine build receipt precede
Python execution, including with parallel Make. Keep standalone `make test` behavior and both
nextest/cargo modes. Retain lightweight preflight, source-integrity, catalog/schema checks,
formatting, lint, every Rust and Python test, feature-on tests and release checks.

Keep the native owner wrapper, explicit selected-profile entry, selective class execution,
metrics, deadlines, budgets and `.af/` authority unchanged. Regenerate protected harness
identities using the repository generator. This refines the full-gate ordering in ADR-0229.

The focused policy regression executes the real wrapper, Make graph and receipt writer with
small command stand-ins, checks all required step labels and stages, and checks both runner
modes and standalone `make test`. A failed no-run build must stop before any suite executes.
Stand-in receipts are isolated fixture output, never gate completion evidence.

## Considered options

- Increase the build allowance: rejected because test execution is charged to the wrong phase.
- Emit an early build marker: rejected because it would not attest to successful compilation.
- Remove or skip the Python suite: rejected because full validation must preserve its coverage.

## Consequences

The full laboratory suite consumes the existing test allowance after a real Rust build. A
slow or failed suite can still fail the gate; no completed full run is inferred from focused
policy tests. Independent gate, review and evaluation remain required on the sealed candidate.
