# ADR-0197: Validate on one latest stable Python

**Status:** proposed (2026-10-23)

## Context

Validation currently repeats the performance laboratory on Python 3.9, 3.11 and 3.13.
The project now wants one Python version in CI, always the latest stable release, including
jobs that exercise Python through Make targets and timing wrappers.

## Considered options

- Keep the compatibility matrix: rejected because it repeats validation for older interpreters.
- Pin a current minor version: rejected because it stops following new stable releases.
- Use the runner's bundled Python: rejected because runner images can lag stable releases.

## Decision

Supersede only ADR-0162's Python CI matrix policy (decision 7 and amendment 7). The laboratory
runs once as `performance lab (latest stable Python)`. Every job in `validate.yml` installs
Python with the commit-pinned `actions/setup-python`, `python-version: '3.x'` and
`check-latest: true`. Prereleases remain excluded by the action's default. The action's
[version selection documentation](https://github.com/actions/setup-python/blob/5fda3b95a4ea91299a34e894583c3862153e4b97/docs/advanced-usage.md)
describes this selection.

Keep both laboratory test and schema-parity commands and require `AF_PERF_REQUIRE_TOMLLIB: '1'`.
The required `lint` check still depends on `performance-python`, runs with `always()`, and first
fails unless that dependency succeeded, including failure, cancellation and skipped outcomes.
Keep Rust validation unchanged. This decision does not change benchmark container images,
vendored TOML parsing or runtime compatibility contracts.

## Consequences

New stable Python releases enter validation automatically; CI therefore depends on interpreter
availability and may expose new incompatibilities without a repository version bump. Older
Python versions no longer receive dedicated CI coverage. Parser parity remains mandatory.
