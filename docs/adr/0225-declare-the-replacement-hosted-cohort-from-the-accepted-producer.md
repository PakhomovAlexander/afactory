# ADR-0225: Declare the replacement hosted cohort from the accepted producer

**Status:** proposed (2026-10-26). Operational declaration under
[ADR-0181](0181-declare-cohorts-before-they-run-inject-only-the-candidate-and-count-whole-power-trials.md),
[ADR-0182](0182-bind-each-calibration-member-to-its-first-dispatch-and-its-authoritys-plan.md)
and [ADR-0184](0184-verify-proposed-activations-in-trusted-ci-and-bind-bridges-groups-and-costs-to-retained-executions.md).
No statistical policy or persisted contract changes; ADR-0194 and ADR-0217 stand.

## Context

The frozen v6 cohort has malformed CPU/image groups and a different workflow producer.
Editing its authority, seeds, history or failed/missing originals would destroy its declaration.
The replacement must use the actual accepted producer and exact observed tuples, with a fresh
complete history anchor and future schedule. Published ADR numbers and decisions remain intact.

## Decision

Declare only `quick_20261028_hosted_v7` in
[`cohort-quick-20261028-hosted-v7.json`](../../fixtures/performance/cohort-quick-20261028-hosted-v7.json)
with the existing `cohort-declare` CLI against the verified read-only Git authority archive.
Authority `508bf45967b8bdcbfaeaae9de27f5258672a617c` has tree
`ee13c8cd0323e853e98ad930e5d84fc20017751a`. Its bytes/modes were mapped to the accepted
implementation-reviewed output of Task `performance-lab-postmerge-perf-cache-repair-20261026`;
this is not separate whole-source verification of the newly created commit identity.
The [operator record](../performance-cohort-dispatch.md#producer-and-supplied-observations)
retains the exact acceptance identities, archive verification, supplied history and group
provenance, and the reproducible command and digests.

Use the six observed CPU/full-image tuples as separate ID, CPU and OCI CLI arguments. Anchor
the repository's `performance.yml` history after run 118, from the complete 118-run capture
at `2026-10-26T17:50:12.704103+00:00`. Preserve every later run. Schedule seven UTC days,
2026-10-28 through 2026-11-03: 20 A/A on days 0–1, 16 preparation on day 2 at 1,000,000
iterations, then 30 descriptive injections and ten whole power trials on days 3–6.
Retain quick/environment/recipe policy, all 28 critical cases, N/K, health, dose bands and
formulas. Derive new seeds with the existing member-seed function and the new cohort ID.
All 20 A/A and at least nine whole power trials must qualify; failed, missing, interrupted,
unhealthy and invalid originals stay in every denominator. Per-case tallies cannot substitute
for whole trials. Never retry, replace or backfill an original.

After A/A and preparation, collect their original artifacts, seal the parameters with
`calibration-prepare`, commit and review them in trusted control before any trial. This remains
a distinct checkpoint; the declaration seals no parameters and claims no empirical readiness.
If that checkpoint misses the fixed trial dates, keep dispatch disabled and seek a new
declaration, preserving this one and all originals.

Keep v6 byte-identical at SHA-256
`62955096a58517dad3544425ead9b18ebe3b85de94b8518d5ce85deb761d5ab7`, disabled for dispatch,
with its archive and historical reader unchanged. Keep `.af/`, runtime, workflows and the
generator unchanged. Regenerate only the protected inventory/staged policy because the new
fixture extends their closure. Do not install that policy into `.af/` here.

The coordinator publishes with a normal merge retaining the actual authority SHA; squash or
rebase would invalidate reachability. Trusted control must retain the exact cohort and producer
bytes. Claw owns the separate disabled-preflight, scheduling and reporting operations described
in the runbook; this Mac needs no periodic collector. This Task installs no service, dispatches
nothing and activates no enforcement.

## Consequences

The CLI reproduces the new declaration exactly. Focused checks cover typed admission,
membership, days, pair counts, seeds, formulas, observed-group matching, old-cohort integrity,
producer equality, generated harness hashes and Markdown. Full native checks, independent
reviews and evaluation are separate enclosing-pipeline obligations. Services, genuine hosted
measurements, parameter sealing, readiness and activation remain later coordinator operations.
