# ADR-0229: Select validation classes and bind release profiles to tags

**Status:** accepted (2026-10-25); gate/release PR and release-preparation defaults superseded in part by [ADR-0239](0239-default-every-pull-request-to-fast-validation.md).

## Context

The supplied validation runs spend over an hour on an undifferentiated suite. A guarded performance
integration test holds all nextest threads for much of that time; Python lab checks are duplicated.
Developers need reviewable functional, workflow, UIX and performance selections with truthful timing
and stable branch-protection check names. Releases must carry the exact selection a release PR chose,
without inheriting moving labels or a prior release's fast policy.

## Decision

Adopt [the complete design](../test-classes.md) and the versioned `.config/test-classes.json` catalog.
Use one stdlib Python resolver/classifier/runner for local and CI selection. Each test has one primary
class; nextest exclusivity remains independent and unchanged. PR defaults select functional, e2e and
uix; local `make check`, ordinary main and unconfigured releases remain full. Positive `test:CLASS`
labels replace defaults, negative `no-CLASS` labels subtract; contradictions, unknown reserved labels
and empty selections fail. `release:fast` supplies fast defaults; `release:full` refuses exclusions
and partial allowlists. Base validation and shipped-binary/archive/signature smoke always run.

Run selected classes in parallel OS/class jobs. The unchanged required `validation / lint` and
`validation / check (ubuntu-latest)` names aggregate selected results and exact plan receipts with
fail-closed handling of missing, skipped, cancelled and failed work. Linux live containers belong to
e2e. The selected perf class owns the Python laboratory on latest stable Python with required parser
parity, feature-on tests, expensive performance integration checks and the pinned-container
telemetry build/measurement smoke. Smoke uses the resolver and must actually succeed in both
stable aggregations; fast/no-perf reports it as NOT RUN. The separate advisory telemetry
producer retains its event and source-binding semantics. No assertion or deadline changes.

Release PR labels directly select validation, without a policy file or metadata commit. Check only
branch/workspace version agreement on PR CI. On first creation of an absent version tag, read the
paginated GitHub commit-associated PR API: only a single merged PR with the exact merge SHA, main
base and `release/vVERSION` head may supply labels. No match defaults full; ambiguous matches,
API failures and invalid selections fail closed. Freeze captured labels, source PR provenance and
resolved plan in the annotated tag before pushing, bound to SHA, version and catalog digest. First
creation observes then-current labels, not their historical merge-time state. Every subsequent main
retry or tag run uses only the immutable annotation, never the API or moving labels. Unbound manual
tags default full regardless of associated PRs or stale metadata. Retain the plan and publish scope
and evidence in release notes. Every selected Linux/macOS gate and packaged binary/architecture/
signature gate must pass before publication.

Provide `make test-labels` as an offline dry-run and `make test-labels APPLY=1` as explicit operator
setup of the ten canonical label definitions. It never assigns PR labels or changes unrelated ones.
Release preparation checks the requested profile label before branch/source/commit/push mutations.
Route native gate Python evidence beneath the existing writable `AF_GATE_NEXTEST_RUN` directory;
ordinary CI/local checkouts retain `target/base-evidence`. Test fixtures use external temporary
sources and prove the actual recording recipes work against an unprivileged chmod seal.
Retain the native gate-owner wrapper and all its ownership regressions. Require its preflight
in base checks and selected class runners; explicit native selection lives in verify-body.sh.
Upstream ADR-0214 through ADR-0223 retain their identities and decisions.

Use the same PR selection in the separate trusted-control Performance workflow: an excluded perf
class is explicitly exempt, not measured. Label changes cancel earlier PR measurements. Measurement
remains advisory, and published-release measurement remains asynchronous; full validation does not
claim to await it. Retain actual JUnit and ci-step metrics, distinguish summed test-seconds and runner
cost from parallel wall time, and label replay bounds as estimates with missing data and provenance.
Class cost and class shares use only outer class intervals; base/container costs remain separate,
while the overall command envelope includes all measured intervals. The timing writer creates the
configured metrics parent for fresh checkouts and preserves the native gate's external output path.

This supersedes ADR-0107's unconditional full CI validation, ADR-0197's unconditional laboratory job
structure, and ADR-0196's prose-only recommendation for a repository operator's `no-perf` label. It does
not change the kernel's prose-only remote-gate label mapping/authority, `.af/`, or protected performance
measurement semantics. Existing `make check` remains full, with feature-on tests moved from lint to
an explicit full-gate dependency.

## Considered options

- Keep an undifferentiated full CI gate: rejected because optional classes cannot be selected or
  attributed and exclusive perf work blocks ordinary iteration. Full local/AF checks remain available.
- Require a committed release-policy handshake: rejected because a label change should itself select
  CI and release validation; a second metadata commit adds an unintended approval step.
- Read labels on every release retry or choose the latest PR: rejected because moving/unrelated labels
  could change the validation of an already tagged release.
- Trust only the first API page or treat API failures as no match: rejected because incomplete evidence
  can hide ambiguity. Scan bounded pages and fail closed on errors.

## Consequences

Fast validation omits perf explicitly. A partial allowlist can omit other classes and therefore has
less coverage, visibly recorded as NOT RUN. Catalog drift fails even on fast selections. Parallel jobs
trade more runner/build work for separation of class critical paths; no guaranteed speedup is claimed.
Tag annotations preserve the release decision across squash/merge/tag/rerun paths. Repository label
management and human publishing authority stay with operators. Historical measurements remain supplied
observations until real JUnit/step evidence is replayed; missing evidence never becomes a zero.

**Amended 2026-10-27 by [ADR-0251](0251-run-the-task-gate-on-the-fast-validation-profile.md):**
This repository's local Task gate now selects the fast validation profile; local `make check`
remains full.
