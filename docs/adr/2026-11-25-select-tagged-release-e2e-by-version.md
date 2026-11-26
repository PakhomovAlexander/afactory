# Select tagged release e2e by version

**Status:** proposed (2026-11-25). Refines tagged selection in
[ADR-0229](0229-select-validation-classes-and-bind-release-profiles-to-tags.md) and
[ADR-0239](0239-default-every-pull-request-to-fast-validation.md).

## Context

The release resolver captures the PR selection in an immutable tag binding. The same release PR
profile can apply to both an RC and its stable promotion. Tagged publication needs different e2e
coverage for these versions.

## Decision

Resolve and validate the tag binding before applying the version rule. Omit the entire e2e class
for `X.Y.Z-rc.N`, including its Linux container probes. Require e2e on Linux and macOS and the
Linux container probes for `X.Y.Z`. A tagged release that has no classes left fails resolution.
The tag's label selection continues to determine the other classes.

Use the same resolver after main creates a tag and when a tag push triggers release. Record the
version decision in the effective plan's e2e row. The shared validator consumes that plan for
the class matrix, container probes and fail-closed aggregation. Signing depends on that aggregate
and the builds. The check summary and release notes use the same e2e reason.

The tagged contexts change the plan contract to `af.test-plan/3`. Tag bindings and class receipts
embed that contract and also move to version 3.

## Considered options

- Apply the rule to release PR checks. This would change validation before the publication
  version and tag are resolved.
- Skip only container probes for an RC. The RC policy applies to all e2e work.
- Infer the version policy independently in each job. That would let validation and release notes
  disagree about the published scope.

## Consequences

The fast profile supplies functional and uix on RC publication and adds the required e2e class
for stable publication. Release PRs and ordinary main pushes keep their existing selections.
Builds, packaging, smoke and pin adoption continue through the existing release workflow.
