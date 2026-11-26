# ADR-2026-11-25-resolve-required-checks-by-workflow: Resolve required checks by workflow

**Status:** proposed (2026-11-25). Supersedes the newest-suite-per-app rule in clause 6 of
[ADR-0275](0275-install-a-durable-github-scenario-adapter.md) for GitHub Actions checks.

## Context

Independent GitHub Actions workflows share the same app ID. A required check can belong to an
older suite than another required check at the same head. A workflow can also run again at that
head before its required check appears, and a rerun can reuse its suite.

## Considered options

- Select the newest matching check name across the app. This would accept old success during
  the gap before the current run or attempt emits its check and would choose between ambiguous
  producers by ID.
- Require workflow IDs in adapter configuration. The repository's exact-head Actions run
  listing already relates each suite to a workflow. Configuring workflow IDs would require a
  new operator contract for the existing check identities.

## Decision

After validating the exact head and configured app, join GitHub Actions check suite IDs to
GitHub's exact-head workflow runs. For each required name, require a unique producing workflow
ID and select that workflow's highest run number. Use GitHub's attempt-specific job listing
to verify check-run membership when the selected run is a rerun. If a suite lacks a workflow
mapping, a producer is ambiguous, or the current run or attempt has yet to emit the check,
keep the observation pending. Require completion of the selected workflow run and continue
to use the check and suite conclusions for readiness and repairable failure.

Use the same resolution for readiness and terminal observations. Keep the per-app rule for
other check apps and the existing exact-context and creator rule for commit statuses.

## Consequences

Required GitHub Actions checks can span workflows. Observing them requires Actions read access
and remains subject to the existing wall-time, request and pagination limits. GitHub Actions
workflows should use distinct required check names. The existing check outcome packet records
the ID and producing suite of each observed check.
