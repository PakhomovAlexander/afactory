# ADR-0184: Verify proposed activations in trusted CI, and bind bridges, groups and costs to retained executions

**Status:** accepted (2026-10-21). Closes package p6b of the
[continuous performance laboratory](../design/continuous-performance.md) (design §5, §8, §12,
§15). It repairs the eleven confirmed findings on
[ADR-0183](0183-detect-deleted-originals-recompute-parameters-and-verify-activations-from-retained-evidence.md)'s
source and the evaluator's missing activation and equivalence deliverables. The accepted design
is unchanged. ADR-0177 to ADR-0183 stand except where this ADR amends them; ADR-0183's
implementation statements on group enforcement, bridge obligations, `--fetch` and the declared
cost cache are superseded here.

| Contract | Version | What changed |
| --- | --- | --- |
| Run | `af.perf-run/6` | an active plan's activation names its exact calibrated CPU/image groups; `/5` is refused |
| Readiness | `af.perf-readiness/5` | each original's `head_sha`; reason `producer_unverified`; `/4` is refused |
| Activation | `af.perf-activation/3` | a bridge names its `equivalence_path` and `equivalence_sha256`; `/2` is refused |
| Equivalence | `af.perf-equivalence/1` | new: a bridge's golden check and post-merge A/A |
| Activation verification | `af.perf-activation-verification/2` | a bridge's verified runs; only owner obligations are pending; `/1` is refused |
| Cost | `af.perf-cost/2` | observed cache, retained identities, execution, failed attempts, distinct completed comparisons; `/1` is refused |
| Shipping | `af.perf-shipping/2` | the reviewed suite, every run once, every planned slot accounted for, `incomplete`; `/1` is refused |

## Context

1. **A huge run number exhausted memory** (`activation-history-numeric-gap`). The history check
   built a set of every number between the anchor and the newest run: a 570-byte listing with
   run number 2147483647 raised `MemoryError` under a 64-MiB address space.
2. **Repeated runs counted twice** (`shipping-duplicate-samples`). The same run directories, or
   byte-identical copies, turned 10 samples per budget into 20 and `demonstrated`.
3. **Failed and missing shipping samples were pooled away** (`shipping-incomplete-attempts-hidden`).
   A failed-shard run missing candidate records beside a complete run reported 39 samples and
   `demonstrated`.
4. **Calibrated groups vanished before enforcement** (`activation-unprepared-cpu-group`). A full
   plan carried only the environment and recipe; synthetic Intel Xeon samples of an explicitly
   uncalibrated group satisfied the required check.
5. **A bridge was admitted without executions** (`equivalent-adapter-admitted-without-execution-evidence`).
   Golden checks and the post-merge A/A were pending strings, yet its full plan satisfied the
   check.
6. **No trusted activation CI existed** (`activation-ci-not-implemented`). Fetching was an
   operator-only `--fetch`; no workflow recomputed a proposed activation from retained evidence.
7. **A capped artifact listing was silently shortened** (`activation-capped-artifact-list-loses-completeness`).
   Ten pages of a 1001-artifact listing were written as `total_count` 1000, hiding a duplicate of
   an expected bundle on page 11.
8. **A bridge's build inputs were not compared** (`equivalent-adapter-build-inputs`). Cargo.lock,
   manifests, build scripts, the toolchain and Cargo configuration could change with the adapter.
9. **Failed cost comparisons counted** (`cost-incomplete-comparisons`). Five runs per group with
   failed builds or shards were `demonstrated`, and the cache was the operator's word.
10. **Shipping took any suite's limits** (`shipping-unrelated-suite`). 100 ms samples planned
    against 50 ms were `demonstrated` under a new 200 ms suite.
11. **The producer of an original was not bound** (`calibration-workflow-producer-source`). The
    run's `head_sha` was dropped; changed workflow or scheduler code could produce a qualifying
    original.

## Considered options

1. **Trigger the activation verifier with `pull_request_target`.** Rejected: candidate code
   must never run in the target context, and the event's policy is not assumed enabled. A
   `pull_request` job that executes only the base revision's code needs no wider trust.
2. **Give the measuring jobs `actions: read`.** Rejected: they build and run candidate code.
   A separate collector job holds the only token.
3. **Raise the page caps.** Rejected: any cap can be exceeded; the cap must refuse.
4. **Deduplicate repeated shipping runs silently.** Rejected: refusing a repeated run or bundle
   keeps the operator's input auditable and cannot hide a conflicting copy.
5. **Let an authored flag or the protected file list admit a bridge.** Rejected by the design:
   executions establish golden equivalence and the A/A; only semantic approval is the owner's.
6. **Require per-group 9/10 power or 20 A/A.** Rejected: the design fixes aggregate cohort
   denominators and reports group tallies descriptively.
7. **Refuse an uncalibrated host before sampling.** Rejected: the evidence stays useful and a
   regression there must stay a failure; the verdict denies the required check instead.

## Decision

1. **Gaps are bounded by the listing** (`calibration._run_history`, `number_gaps`). Distinct
   listed numbers are contiguous exactly when their count equals the span; gaps are named from
   the sorted listed numbers, at most eight ranges. Deleted-original detection is unchanged.
2. **The producer is bound** (`calibration.producer_check`). Every listed run must name a full
   `head_sha`; the attempt record and every measuring job must agree. An original whose head
   commit's `performance.yml`, or whose plan's control revision's `scripts/performance`, is not
   byte-identical to the cohort authority's, or cannot be shown, is `invalid` with
   `producer_unverified`. The head need equal neither the control nor the authority.
3. **Listings are complete or refused** (`activation._complete_listing`). Jobs and artifacts
   are read within their page bounds; a listing longer than the bound, short of or over its
   `total_count`, changing its count between pages, or naming an item twice is refused. The
   written `actions.json` count is the observed one, and `calibration` refuses a listing shorter
   than its count or with a repeated artifact.
4. **Plans carry calibrated groups, and verdicts bind hosts to them** (`planning._activation_record`,
   `compare._calibrated_hosts`). An active plan names each calibrated group's exact CPU model and
   image. In an eligible plan, a measured shard observed on another or an unknown host adds
   `uncalibrated_cpu_group` at the incomplete level: the run cannot satisfy the required check,
   a regression stays a failure, and the report names the reason. Unprepared groups stay listed
   as uncalibrated; denominators are unchanged.
5. **A bridge needs verified executions** (`perflab.equivalence`, `activation.bridge_evidence`).
   - **The build closure.** The protected diff also covers every `Cargo.lock`, `Cargo.toml`,
     `build.rs`, `rust-toolchain(.toml)` and `.cargo/**`, additions included, and both
     authorities must hold `Cargo.lock`.
   - **The golden check.** A retained bootstrap run measured by the bridged authority, baseline
     the calibrated product source and candidate the bridged one, under the calibrated policy,
     tier, environment and recipe: both builds succeed with adapters of the group, the bridged
     product through the bridge's adapter, and every planned sample of every cohort case is
     present and accepted by its oracle.
   - **The post-merge A/A.** A retained run of one source descending from the bridge, through
     the bridge's adapter on both sides, on calibrated groups, healthy and complete, every
     critical case within tolerance.
   - **The record.** `equivalence-evidence` writes `af.perf-equivalence/1` with each run's
     Actions run, explicit lab run, plan, verdict, build, sources, digests, adapters, probe
     binaries, groups and case outcomes. Its reasons are re-derived by the reader. Reviewed
     verification requires its context to be exactly the manifest's, both checks verified and
     the A/A source to descend from the bridge; proposed verification refetches both runs and
     requires the record byte for byte. Owner semantic approval stays pending.
6. **Trusted activation CI** (`.github/workflows/performance-activation.yml`,
   `cli.py activate --verify-proposed`). On a pull request to `main` touching
   `fixtures/performance/`, one job with `contents: read` and `actions: read` checks out the base
   revision and runs its `cli.py` with `python3 -I`. `proposed_changes` compares the manifest and
   every record it names with the base. When anything changed, the cohort's evidence and a
   bridge's runs are fetched and recomputed; otherwise reviewed verification runs locally and
   nothing is fetched. The token reaches that step alone. `--verify` keeps the reviewed path,
   and `--fetch` is removed.
7. **Costs are bound to retained executions** (`costs.run_cost`). The cache is read from the
   build manifest's dependency fetch (prefetched is warm); a declaration must agree and only
   names the cache of a run without a manifest. Each run records its sources, plan, bundle,
   build, binary and shard identities. A failed build or shard, a missing shard or missing
   records make a failed attempt, listed in `failed_attempts`. Groups count distinct completed
   measured comparisons; medians use completed runs. A completed regression counts.
8. **Shipping is bound to one reviewed suite** (`costs.shipping_report`). Every run must have
   frozen the suite's policy digest and budgets; a run or bundle given twice is refused. Every
   planned measured candidate slot is a sample, a failure or missing, and a shard that did not
   complete leaves its instances `incomplete`.

## Consequences

- **Tests.** Each finding has a negative that fails on the predecessor and passes here:
  - `test_calibration.Closeout`: run number 2147483647 refuses with a bounded range; changed
    workflow and scheduler commits are refused while a different head with the reviewed
    producer qualifies;
  - `test_activation.Listings`: the 1001-artifact listing, doubled or inconsistent pages and an
    incomplete job listing are refused;
  - `test_activation.EquivalentAdapter`: a bridge with verified runs is admitted; oracle
    failures, another adapter, one product source, an uncalibrated host, an A/A regression, a
    control-authority run, forged or unbound records and an expired bundle are refused; changed
    lock, manifest, build script, toolchain and Cargo configuration need a new cohort;
  - `test_planning`: Intel Xeon, unknown, other-image and one-shard hosts cannot satisfy the
    check, the prepared EPYC group still does;
  - `test_performance_workflow.ActivationWorkflow`: permissions, token scope and the base's code
    are checked, and the steps run against a disposable repository;
  - `test_costs`: failed builds, failed and interrupted shards, missing shards, a completed
    regression, repeated comparisons and contradicted caches; repeated and copied shipping runs,
    failed and missing slots, interrupted shards and an unrelated suite.
- **Fixtures.** Schema fixtures and index entries cover every changed contract; bridge and cost
  scenarios are generated at test time.
- **Pending operational evidence.** No workflow was dispatched and no authenticated API read.
  The five-day hosted cohort, its proposed activation run by the hosted workflow, a real bridge,
  real PR and same-source cost runs, named-host shipping samples, the staging handoff and
  ruleset coexistence remain operational evidence of p8.
- **Not changed.** The 15-second lease, the 8-MiB envelope cap, aggregate denominators,
  whole-trial readiness, the measuring workflow's permissions and trust boundaries, release
  handoff and deduplication, and advisory enforcement.
