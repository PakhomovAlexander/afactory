# ADR-0213: Isolate CLI usage tests from the repository release pin

**Status:** proposed (2026-10-25).

## Context

The `0.13.0-rc.1` source version legitimately differs from the repository's signed release
pin until publication ([ADR-0138](0138-the-repository-pins-its-newest-release.md)). An offline
source build warns before parsing when it cannot dispatch to that pin. The syntax-only
`usage_errors_under_json_print_one_error_document` test inherited the checkout as its working
directory, so that warning displaced the clap error it expected on stderr's first line.

## Decision

Run every invocation in that test from one empty temporary directory, through
`crate::common::af()` with its private HOME/XDG directories and offline setting. Retain every
existing assertion about exit codes, plain output, the single schema-valid JSON error document,
command identity, next action, help and version.

## Considered options

- Move the pin to the source version: rejected because signed release bytes exist only after
  publication, and the release workflow owns the pin.
- Suppress dispatch warnings or loosen the stderr assertions: rejected because the warning is
  correct and the usage-output contract still needs exact coverage.
- Change every CLI surface test's working directory: rejected because only this syntax fixture
  needs isolation; dispatch and pin behavior have their own tests.

## Consequences

The usage fixture tests parsing independently of the release cycle. The signed pin, release
workflow and CLI dispatch behavior retain their existing contracts.
