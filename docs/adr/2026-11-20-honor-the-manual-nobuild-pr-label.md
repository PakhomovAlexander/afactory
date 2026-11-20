# ADR-2026-11-20-honor-the-manual-nobuild-pr-label: Honor the manual nobuild PR label

**Status:** proposed (2026-11-20). Supersedes the always-on PR base validation requirement in
[ADR-0229](0229-select-validation-classes-and-bind-release-profiles-to-tags.md) for `nobuild` PRs.

## Context

An AF pin refresh can change only `.af/af.lock`. Hosted validation still builds the workspace,
runs the base test suites and expands the release branch's Linux and macOS test matrices.

## Considered options

- Classify changed paths in CI. This would require maintaining rules about which files can
  affect builds and tests.
- Exclude every validation class through existing selection labels. Base checks would still
  run, and the class resolver deliberately refuses empty selections.

## Decision

Authors may apply `nobuild` when the entire pull request changes no sources, pipelines, build
inputs or tests. A change solely to `.af/af.lock` qualifies. Authors review the full diff after
updates and remove the label when it ceases to apply. GitHub job conditions read the label on
the PR event, including on `release/` branches.

## Consequences

The required validation checks report the exemption, `lint` still checks pin metadata, and
the PR report still runs. Hosted build, smoke, test and performance jobs skip. Removing the
label restores the ordinary selection. The AF local Task gate, main and tag validation and
release evidence requirements continue to apply.
