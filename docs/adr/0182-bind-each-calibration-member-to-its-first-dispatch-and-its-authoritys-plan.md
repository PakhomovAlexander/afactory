# ADR-0182: Bind each calibration member to its first dispatch and its authority's plan, and give each probe setup write its own lease

**Status:** accepted (2026-10-20); unconditional member naming amendment proposed in
[ADR-0208](0208-bind-calibration-dispatch-to-the-checked-main-source-and-separate-group-arguments.md).
Closes the calibration part of package p6b of the
[continuous performance laboratory](../design/continuous-performance.md) (design §12, §15).
It repairs three findings and three gate failures left on
[ADR-0181](0181-declare-cohorts-before-they-run-inject-only-the-candidate-and-count-whole-power-trials.md)'s
source, and executes the calibration builds natively. Activation verification is a later p6b
Task. ADR-0177 to ADR-0181 stand except where this ADR amends them.

| Contract | Version | What changed |
| --- | --- | --- |
| Cohort | `af.perf-cohort/3` | `dispatch`: the repository, workflow, branch, event and run name of every member run; `/2` is refused |
| Readiness | `af.perf-readiness/3` | a member's provenance is its first dispatch and attempt 1, with nullable run-bundle fields; new missing and attempt reasons; refused evidence names its source, dispatch or artifact; `/2` is refused |

The run plan (`af.perf-run/5`) and the calibration parameters
(`af.perf-calibration-parameters/2`) keep their shapes.

## Context

1. **A member's original was chosen among readable bundles**
   (`p6b-calibration-original-dispatch-binding`). `calibration.collect` read each run bundle,
   dropped the unreadable ones, and only then took a member's lowest Actions run ID. Making two
   failed power originals' manifests unreadable let two later successful dispatches stand in
   for them: 8 of 10 qualifying trials became 10 of 10, and the cohort became ready.
2. **A plan was judged against itself** (`p6b-calibration-frozen-plan-crosscheck`). A plan that
   raised one critical case's N by two, with its rank, tail and order regenerated, or one with
   another policy digest, validated as a plan. Its member was complete and qualified.
3. **The region's validator was warm** (`p6b-calibration-authority-cache-warmup`).
   `CapturedTaskPlanValidator` keeps the last structurally validated Task and plan. ADR-0181's
   setup captured one authority, proposed and admitted through it, then timed the region's
   first plan check through the same one. That check reused the setup's validation.
4. **One setup write can take most of a lease** (`p6b-calibration-gate-setup-lease`). The gate
   failed `authority_instances_at_both_package_sizes_pass_the_oracle` for the large package.
   Admission was stamped 14.5 s after the proposal, past the lease opened 2 s before it.
   - **Reproduced here.** At load 37 the proposal took 7.1 s. One Task projection alone took
     4.4 s, and admission then failed the same way.
   - **The cause.** A sample of the process showed the time in parsing the 8 MiB author
     envelope into a JSON value, canonicalizing it and formatting its integers. Every
     projection, and every write's reference check, does that again.
   - **What does not help.** `serde_json`, `sha2` and `review-store` are already optimized in
     the dev profile. An admission holds the Store for two or three of those passes, so neither
     a renewal before it nor the production heartbeat can keep a 15-second lease through one
     write.
5. **Two heavy review tests ran out of real time** (`p6b-calibration-gate-review-history`).
   - **The stale case.** One reviewer write failed with `Task write lost its sequence/lease
     comparison`. The heartbeat renews through its own connection once a Store operation has
     held the shared one into the lease's last 4 s (ADR-0128). The work's next append then
     loses its sequence comparison.
   - **The rediscovered case.** A reviewer was left 377 ms of its 5-second Attempt wall after
     its preparation, and was killed.
   - **Alone.** Both are correct answers to a fixture that spent its real-time budget on
     scheduling. They pass together in 25 s alone and took 67-69 s on the loaded gate.
6. **No calibration build had executed.** The loop's control was only refused by verdict builds
   and exercised by core unit tests.

## Considered options

1. **Pick the original among readable bundles, or from the artifact listing.** Rejected: an
   unreadable, expired or never-uploaded original then vanishes. The artifact API has no
   attempt, and a rerun moves a run's latest `run_attempt`.
2. **Write Actions run IDs into the cohort after dispatch.** Rejected by ADR-0181: the cohort's
   bytes and digest are fixed before execution.
3. **Check a plan's fields one by one against the cohort.** Kept, but not sufficient: the
   cohort declares no instrument, fixture, lock or limits identity. A plan must also be the
   plan the trusted authority freezes for that member again.
4. **Keep one setup authority and relabel the region warm.** Rejected: the measured contract is
   the first check of a fresh process.
5. **Renew in setup, run the heartbeat in setup, lengthen the lease or retry.** Rejected: a
   write that outlives its lease must fail, and the lease stays 15 s. A setup lease that lapsed
   and is quietly replaced by a fresh one is resurrection, which ADR-0181's `hand_over`
   tolerated; that tolerance is removed.
6. **Raise the heavy tests' walls or Attempt counts.** Rejected: ADR-0124 sanctions only
   running a real-time-dependent test alone.

## Decision

1. **A member's original is its first dispatch** (`calibration.collect`, `_dispatches`).
   - **Naming.** `performance.yml` names a calibration dispatch `af-perf calibration <cohort
     digest> <member>` (`run-name`, from the new `cohort_digest` and the `member` inputs) as
     the run is created, before any job. `ci-schedule` refuses a dispatch whose digest is not
     the control's cohort, and that run stays the member's failed original.
   - **The trusted producer.** The cohort's `dispatch` names it: a `workflow_dispatch` of
     exactly `.github/workflows/performance.yml` on the declared branch of the declared
     repository. A run named for this cohort by any other producer is `foreign_dispatch`.
     One named for an undeclared member is `unknown_member`, and one of another cohort's digest
     is `other_cohort`. None of them binds anything.
   - **The history.** `runs.json` is the filtered workflow-run listing
     (`event=workflow_dispatch`), all pages. A listing short of its `total_count`, beyond the
     1000-result cap, listing a run twice, or whose lowest run ID is not the earliest dispatch
     of a member, refuses to judge anything.
   - **The original.** A member's first such run is its original, at attempt 1, whatever became
     of it. Later dispatches of the member are `replacement`, and they and every artifact they
     upload are refused.
   - **The attempt.** `attempts/<run>.json` is GitHub's record of attempt 1. Its own start places
     the member on its day and after every A/A and preparation attempt's end. An artifact's
     upload time no longer decides either. An absent or mismatched record is
     `attempt_unavailable` (invalid), and an unfinished one `attempt_incomplete`.
   - **The evidence.** Only then is `af-perf-gh<run>_a1_primary-run` looked up. Absent, expired
     or unreadable, the member is `missing` with `artifact_missing`, `artifact_expired` or
     `artifact_unreadable`, its provenance kept. A member's own dispatch whose plan names another
     member or cohort is `wrong_plan`.
2. **Every plan is the authority's plan for its member** (`replanner`, `_replanned`,
   `_cohort_mismatch`).
   - **Planning again.** Readiness and preparation freeze each member's plan again from a
     repository, `--repo`: the cohort's authority registry, instruments, fixtures and locks.
     They use the retained run ID, control, shard count and build. The plan must equal the
     retained one in everything but its selector text and digest.
   - **Field checks.** The plan's case family, criticality and N, family size, policy digest,
     authority and mode, health rule, tier, environment and recipe are also checked against the
     cohort.
   - **A slowdown member** is planned with the sealed parameters readiness judges by, so its
     injection is the one those parameters give.
3. **The region checks through a cold authority** (`authority::Region::capture`,
   `require_cold`). The setup writes' host and authority serve only proposal and admission and
   are dropped. After setup, outside every lease, the region captures a new host and authority.
   `CapturedTaskAuthority::holds_validated_plan` (and the validators' own) shows whether a check
   would reuse a validation. The probe refuses to time a region through one that would.
4. **Each setup write owns a whole lease** (`authority::build_then` with `Setup::hand_over`).
   - **The writes.** The opening lease covers the proposal and is released. A lease taken at the
     next epoch covers the admission and is released. The region takes its own lease last, as
     before.
   - **Releasing.** Every release happens while the lease is live. A lease the Store reports
     lapsed is the side's `lease_expired` failure before the region. No later lease replaces it.
   - **Unchanged.** The plain fixture, `build`, still writes exactly opened, proposed and
     admitted. The expiry tests keep its lease and let it lapse.
5. **Real-time tests run alone** (`.config/nextest.toml`).
   `authority_instances_at_both_package_sizes_pass_the_oracle`,
   `heavy_review_cannot_erase_stale_claims` and `heavy_review_cannot_erase_rediscovered_claims`
   hold every test thread, each with its measured reason. Their assertions, exit codes,
   acceptance, Attempt counts, walls and leases are unchanged.
6. **The calibration builds execute** (`make lint`, `test-perf-calibration`). The step builds
   `perf_probe` with `perf-calibration`. Its `calibration_build` tests build the probe and `af`
   with the feature through the shared executable boundary (`built_with_features`) and run them.
   - **The probe.** No control is no injection and no report. `0` and a count are one injection
     and one last stderr line for exactly that count. A nonzero injection lies inside the
     region's measured wall, and the oracle's digest is unchanged.
   - **Refusals.** Malformed and over-2^40 counts exit 2 before any loop, within a minute.
   - **`af`.** With a private HOME and XDG directories, `--version` and `--help` keep their
     stdout byte for byte and add one report line.
   - **Verdict builds** still refuse the control (`cli_surface`, `perf_probe`).

## Consequences

- **Tests.**
  - **`test_calibration`.** An original that failed before uploading anything keeps its
    identity beside a later success. So do unreadable, expired and omitted originals. The
    predecessor's counterexample, two failed power originals made unreadable beside two
    successful replacements, stays 8 of 10 and not ready. Incomplete, capped, doubled and
    ambiguous histories are refused. Attempt records, not uploads, decide schedule and order.
    Self-consistent plans with N + 2, another policy digest or another instrument digest are
    `wrong_plan`, while an unchanged plan qualifies.
  - **`test_planning`.** A plan frozen again from a real repository equals the dispatched plan.
    It differs after a changed fixture digest, lock, limit or seed.
  - **`test_ci` and `test_performance_workflow`.** A dispatch digest other than the control's
    cohort is refused. The run name, the new input and the attempt-1 guard are checked.
  - **`perf_probe`.** Each setup write's lease begins after the delay before it and ends before
    the delay after it. A setup lease lapsed before its write fails `lease_expired` with only
    `opened` written. The region's authority is cold until its first check and refused after.
    The large-package oracle test passed alone in 116 s at load 60-80 from other processes.
- **Pending empirical evidence.** Nothing here ran on a hosted runner, dispatched a workflow or
  read an authenticated API. The workflow-run and attempt fixtures follow GitHub's documented
  shapes and one public response, and establish orchestration only. A real cohort over five
  days, its provenance, measured A0 and loop costs, shipping budgets and activation verification
  remain operational evidence.
- **Not changed.** The 15-second lease, the 8-MiB envelope cap, real expiry and fencing, the
  production heartbeat, verdict statistics, health rules, the whole-trial readiness rule, the
  workflow's rerun guard and advisory enforcement.
