# ADR-2026-11-13-bind-release-ci-and-retry-bytes: Bind release CI and retry bytes

**Status:** proposed (2026-11-13).

## Context

The developer release controller must work when CI is not formally branch-required, while
refusing a merge whose approved base can race. Partial publication must retain its signed bytes.
This extends the [developer release decision](2026-11-12-own-release-cuts-in-a-repository-dev-cli.md).

## Considered options

- Treat an empty required-check list as success: rejected because actual release CI is mandatory.
- Trust head matching to protect base: rejected because it only constrains the candidate head.
- Rebuild or overwrite draft assets: rejected because timestamps and signing provenance can differ.

## Decision

Bind the reviewed CI workflow, resolver and Linux/macOS aggregate policy in source. Admit only
latest-attempt successful exact-head PR-associated evidence and additional required statuses.
Retain declared optional skip semantics without reporting skipped work as measured passes.
Require an administrator-provisioned active strict main status ruleset without bypass before
planning can authorize work. Never modify repository settings from the controller.

Retain the complete signed asset bundle before publication and restore it across workflow runs.
Preserve the original signing provenance while requiring a successful protected publication join.
Missing pin credentials affect only pin readiness. Version the run and policy shapes as generation
2; generation 1 is refused, with no migration or fallback.

## Consequences

The current repository needs separately authorized administrator setup before cutting a release.
Read-only plan explains the missing prerequisite. Exact bundle retention bounds automatic recovery
to 90 days; recovery after expiry requires the original bytes. The release gate runs the actual
telemetry contract regression in every profile; this is deterministic coverage, not a measured
performance claim. Fixture declarations remain internally consistent across RC preparation.
