# ADR-2026-11-27-recover-release-ci-duplicates-and-inventory-reads: Recover release CI duplicates and inventory reads

**Status:** proposed (2026-11-27).

## Context

The fast release PR for `v0.15.3-rc.1` had two exact-source CI runs. The run with the higher ID
was canceled before retaining its plan. The passing run supplied complete evidence, but the
controller waited on the canceled run until an operator reran it. Publication later encountered
different complete GitHub inventory reads just after draft creation.

## Considered options

- Keep labeling fast PRs and rerun the highest-ID run. This repeats validation even when the
  passing run proves the same source and profile.
- Accept any prior success for the head. This can hide a newer failure, a rerun, or different
  base or profile evidence.
- Trust the most recent inventory traversal. This loses the consistency check before
  publication effects.

## Decision

This narrows the run-selection requirement in the
[release controller decision](2026-11-12-own-release-cuts-in-a-repository-dev-cli.md) and
[CI and retry decision](2026-11-13-bind-release-ci-and-retry-bytes.md).

Open fast PRs without a profile label because the release PR resolver already defaults to fast.
Continue to recognize existing fast-labeled PRs and require the full label for full PRs.

Select the latest exact run after excluding first attempts that were canceled before retaining a
plan. Re-observe the same selection before merge. The selected run must still supply its latest
attempt's exact source, base, tree, profile and complete successful job roster. All formally
required checks and the strict main rule apply.

During publication, retry differing complete release inventory reads twice with bounded delays.
Each attempt requires two matching complete traversals. Retain the existing draft, source and
signed bundle identity checks.

## Consequences

A canceled duplicate without a plan can cease blocking a valid passing run. An in-flight run,
a canceled rerun, or evidence for a failed authorized profile can still block merge. Concurrent
inventory changes can settle within one publication invocation. Persistent disagreement or invalid
pagination continues to stop publication.
