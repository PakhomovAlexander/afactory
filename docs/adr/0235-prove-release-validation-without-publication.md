# ADR-0235: Prove release validation without publication

**Status:** proposed (2026-10-27).

## Context

Operators need repeatable hosted evidence for fast and full release validation on Linux
and macOS before publishing a release. Production class selection and immutable tag binding
already exist under ADR-0229. A separate worktree is changing production profile defaults;
this proof must exercise the current functions without changing those defaults.

## Considered options

- Publish a trial release: rejected because validation proof requires neither publication,
  signing authority nor remote tag writes.
- Copy the release gate jobs: rejected because copies would drift from required base,
  container, artifact and aggregate checks.
- Simulate the entire gate locally: rejected because that cannot demonstrate hosted
  platform execution. Local resolver fixtures still prove binding semantics separately.

## Decision

Add a read-only proof caller of the existing `validate.yml`, passing `release: true` and an
explicit fast/full plan. Bind that plan to the checked-out `github.sha`, workspace version
and catalog through the production policy functions. Permit manual dispatch and, for initial
pre-merge evidence, ordinary pull request events filtered to the proof workflow path with
an explicit release profile label. Reuse pinned actions without secrets or write authority.
Keep production defaults and all existing workflows unchanged.

Run the real resolver fixture suite in disposable local Git repositories with fixture API
responses. Retain its results as local binding simulation, including frozen retry selection,
SHA/version/catalog and tag-target refusals, and full fallback. Retain the validator's
selected-class timing evidence separately. Fast records perf as NOT RUN; no publication
or savings measurement is inferred.

## Consequences

The operator owns live dispatch and assessment of both hosted runs. A local focused test
pass does not establish hosted success, release publication, independent acceptance or a
full gate pass. Task-specific instructions require focused checks only in this implementation;
the kernel runs its own sealed-candidate gate. The proof introduces no persisted kernel shape,
new publication authority or `.af/` change.
