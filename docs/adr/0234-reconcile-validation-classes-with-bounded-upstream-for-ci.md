# ADR-0234: Reconcile validation classes with bounded upstream for CI

**Status:** proposed (2026-10-27).

## Context

The sealed four-class validation candidate predates upstream changes from
`62f8ecd006cddceefcdef8ae1f0de52a41b13626` through
`91f75edbdf58db70a2ca365fd9159ee789f658e1`. The user requests GitHub CI on the combined
candidate, preserving the prior repairs and upstream contracts. This implementation Task
only edits its sandbox; the operator owns publication of the unverified candidate.

## Decision

Apply the bounded upstream patch after verifying its supplied SHA-256. Retain upstream's
profile v6 contract, retained-symbol archive admission, diagnostic settings, replacement
calibration cohort, guarded-consumer fixtures and README. Preserve upstream ADR identities
0224, 0225, 0227 and 0228. Move only the unmerged validation feature's ADRs 0224–0228 to
0229–0233, updating their references; the upstream numbering gap stays intact.

Keep the functional/e2e/uix/perf catalog, label resolver, immutable release tag selection,
fail-closed aggregation and both perf smoke jobs. Preserve the regression requiring both
smoke dependencies, their runtime selection and separate cost attribution. New upstream
laboratory tests remain perf under its discovery rule; guarded experiment integration tests
remain in the catalog's perf module. No new test binary or top-level script is introduced.

Preserve the macOS fixture repairs, measured-boundary interruption fixtures, genuine Rust
build before Python performance tests, and the unchanged assertions, gates and budgets.
Regenerate protected inventory and staged policy with `harness-digest --write`, rather than
copying upstream hashes over the combined closure. Leave `.af/` unchanged.

## Consequences

Run focused schema, catalog, selection, workflow and harness checks in this Worker. The
specific Task requirements prohibit repeating full Rust/Python suites, clippy and `make check`
locally. Focused checks establish neither a full gate pass nor independent review acceptance,
real contained smoke success or performance measurement. The kernel seals the sandbox;
publication and GitHub CI remain operator actions. This decision grants no release, push,
pull request, label or machine-configuration authority.
