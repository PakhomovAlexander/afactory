# ADR-0239: Default every pull request to fast validation

**Status:** proposed (2026-10-27). Supersedes the PR and release-preparation defaults in [ADR-0229](0229-select-validation-classes-and-bind-release-profiles-to-tags.md) and the gate PR default in [ADR-0233](0233-select-real-af-runtime-smoke-with-the-perf-class.md).

## Context

The test-class catalog and GitHub labels are in place. ADR-0229 made ordinary PRs fast, but gate
PRs and release PRs without labels still ran all four classes. Release preparation also attached
`release:full` unless the operator selected fast. These exceptions make the requested fast track
dependent on PR type.

## Decision

Every pull request defaults to functional, e2e and uix. This includes gate PRs targeting
`af-gate/**` and release PRs named `release/vX.Y.Z`. Explicit positive and negative class labels
retain their existing precedence, and `release:full` still selects all four classes. Release
preparation defaults to `release:fast`; `PROFILE=full` remains available.

A tagged release from an exact matching merged release PR inherits that PR's label selection,
including the fast default when the PR has no labels. Its immutable tag binding retains that
choice for retries. Ordinary main validation, full local `make check`, releases without a
matching merged release PR and direct unbound tags retain their full default.

Record release PR selection in a distinct `release_pr` context, so a release without PR
provenance cannot acquire fast defaults. The added context changes the persisted plan shape:
write `af.test-plan/2`, `af.release-tests/2` and `af.test-class-receipt/2`, updating their schemas
and parity tests together. A release binding requires `release_pr` exactly when it carries
matching PR provenance; otherwise it requires the full, unlabeled `release` plan. Current
bindings replay the recorded plan without consulting moving labels. Earlier contract versions
fail closed under ADR-0145; they never silently become unbound full selections.
Existing tags are never rewritten; their tagged source retains its resolver and recorded selection.

## Considered options

- Keep gate and release PRs full by default: rejected because those PRs would continue to bypass
  the requested fast track solely because of their branch names.
- Change the default for every release and main run: rejected because the request concerns PRs;
  commits and unbound tags have no matching PR selection to inherit.
- Make the release script fast while leaving unlabeled release PRs full: rejected because a
  manually opened release PR would still differ from the requested PR default.

## Consequences

Perf validation and the separate PR Performance measurement are excluded by default for every
PR, with their existing NOT RUN and exempt reporting. Operators can select perf or full validation
with labels. Tagged releases with a matching PR carry that selection; unbound release paths still
run all classes. Base checks, selected-class aggregation and packaged-binary smoke are unchanged.
