# ADR-0183: Detect deleted originals, recompute parameters, and verify activations from retained evidence

**Status:** accepted (2026-10-21). Completes package p6b of the
[continuous performance laboratory](../design/continuous-performance.md) (design §5, §12, §15).
It repairs the five published review findings and the deleted-original finding left on
[ADR-0182](0182-bind-each-calibration-member-to-its-first-dispatch-and-its-authoritys-plan.md)'s
source. It adds activation verification, equivalent-adapter admission and the cost and
shipping-budget demonstrations. ADR-0177 to ADR-0182 stand except where this ADR amends them.

| Contract | Version | What changed |
| --- | --- | --- |
| Cohort | `af.perf-cohort/4` | `dispatch.after_run_number`: the workflow's run number when the cohort was declared; `/3` is refused |
| Readiness | `af.perf-readiness/4` | each original's `run_number` and measuring `runner`; the `history` it was bound from; `recomputed_calibration_digest`; reasons `attempt_unsuccessful`, `artifact_ambiguous` and `runner_unverified`; `/3` is refused |
| Activation | `af.perf-activation/2` | cohort, parameter and readiness records by path and SHA-256; the calibrated environment, recipe and groups; a bridge names its retained calibrated manifest; `/1` is refused |
| Activation verification | `af.perf-activation-verification/1` | new |
| Cost | `af.perf-cost/1` | new |
| Shipping | `af.perf-shipping/1` | new |

## Context

1. **A deleted original was replaced** (`p6b-closeout-deleted-original-dispatch`). Deleting a
   run removes its record, its attempts and its artifacts. The remaining filtered history was
   complete by its own `total_count`. Deleting two failed power originals left two later
   successful dispatches as the lowest surviving runs, and 8 of 10 became 10 of 10.
2. **Unsuccessful attempts qualified** (`failed-original-attempt`). A completed attempt-1 record
   with conclusion `failure`, `cancelled`, `timed_out` or none still counted when its bundles
   were complete. Its A/A and preparation samples also fed A0 and the loop's cost.
3. **Parameters were trusted as supplied** (`unverified-derived-parameters`). A self-consistent
   document with A0 200 ms, every target, count, plan and dose regenerated, was 10 of 10 ready.
   The retained A/A originals give 100 ms.
4. **The first of two artifacts was chosen** (`duplicate-original-artifacts`). Two listed
   artifacts of one expected run or shard name, with distinct IDs, left the first qualifying.
5. **Runner provenance was not checked** (`fresh-hosted-runner-provenance`). A cohort runs on
   fresh hosted runners, but `ci-schedule` planned a member on a configured dedicated runner, and
   collection never looked at where the measuring jobs ran.
6. **An impossible date raised a traceback** (`invalid-calendar-evidence`).
   `2025-02-30T06:00:00Z` matched the timestamp pattern and `strptime` raised `ValueError`.
7. **Activation, equivalent adapters and costs had no executable support.** The manifest named
   readiness bytes nobody rehashed. No command fetched the cohort or checked a bridge's diff, and
   no command summarized the design's cost and shipping obligations.

## Considered options

1. **Trust a mapping of members to runs written after dispatch.** Rejected: an author writes it
   after outcomes can be known, and nothing shows that it is complete.
2. **Require the whole run history from run number 1.** Rejected: one unrelated deletion, or
   retention of old runs, would refuse every later cohort, and long histories exceed any bound.
3. **Anchor the history at the schedule's start date.** Rejected: a dispatch before the start,
   then deleted, would be invisible.
4. **Recompute parameters and substitute them into the trials.** Rejected: trials ran with the
   loop counts they were dispatched with, and substitution retunes after outcomes.
5. **Treat a renamed label or a self-written runner field as provenance.** Rejected: labels alone
   are not evidence, and the artifact API has no runner.
6. **Compare a fetched recomputation with the record byte for byte.** Rejected: the history grows
   and later refusals accrue without changing any judgment.

## Decision

1. **The cohort declares a history anchor; a gap refuses** (`calibration._run_history`).
   - **The anchor.** `cohort-declare --after-run-number N` records the performance workflow's
     latest run number before any member exists. The cohort's digest is in every member's run
     name, so no dispatch can precede the declaration.
   - **The history.** `runs.json` lists runs of every event, newest first, down to a run at or
     before the anchor. GitHub numbers a workflow's runs in order and never reuses a number, so
     every number after the anchor must be listed. A gap is a deleted run and refuses the
     history. So does a run named for the cohort at or before the anchor, or a listing that ends
     above it.
   - **Why a missing first dispatch cannot be replaced.** Any later run, a replacement included,
     shows the gap. A deleted run newer than every listed one has no later dispatch to stand in
     for it, so its member has no dispatch and stays missing. Absent attempt or job records
     leave the original invalid with its provenance, never replaced.
   - **Order.** A member's dispatches are ordered by run number, which must agree with their IDs
     and creation times.
2. **Only a successful attempt counts** (`_original_attempt`). A completed attempt 1 with any
   conclusion but `success`, including none, is `failed` with `attempt_unsuccessful`. The member
   keeps its provenance and its place in the denominators. Preparation reads complete members
   only, so such an attempt supplies no A0 and no loop cost.
3. **Parameters are recomputed before any trial is judged** (`recompute_parameters`).
   `readiness` runs `prepare` on the A/A and preparation originals. The result must be exactly
   the sealed document every trial names, or nothing is judged. Readiness records the recomputed
   digest, and its reader requires it to equal the sealed one.
4. **Ambiguous artifacts invalidate the member** (`collect`). When an original's expected run or
   shard bundle name is listed twice or more, every one of those artifacts is refused as
   `duplicate`, and the member is `invalid` with `artifact_ambiguous`, in any listing order.
5. **Measuring jobs must be shown on fresh hosted runners** (`_measuring_runner`, `ci._dispatch`).
   - **Scheduling.** `ci-schedule` refuses any calibration dispatch with the dedicated executor
     (`dedicated_calibration`), configured or not. `cohort-declare` refuses an environment that is
     not `github_hosted`.
   - **Collection.** `jobs/<run>.json` is attempt 1's job listing. Each planned `build` and
     `shard` job must appear exactly once, at attempt 1, completed, with labels exactly the
     plan's hosted label, runner group `GitHub Actions` and a runner named `GitHub Actions N`.
     The trusted producer and the replanned plan are checked as before. Anything else,
     including a missing record, is `runner_unverified`.
6. **An impossible instant is invalid evidence** (`_unix_ms`). The parse error becomes a
   `ContractError`. A bad attempt record leaves the member `attempt_unavailable`, and a bad run
   or artifact time refuses collection with exit 2.
7. **Activation is verified, never trusted** (`perflab.activation`, `cli.py activate --verify`).
   - **The manifest.** It binds the cohort, parameters and readiness as files of the control
     revision by path and SHA-256, every original run, and the calibrated environment, recipe
     and CPU/image groups.
   - **Reviewed verification.** Every active plan runs it (`planning.verified_activation`), as
     does an ordinary pull request. The records are rehashed, and readiness is validated against
     its cohort and parameters, which recomputes every tally and the decision. It must be ready.
     Its originals must be exactly the manifest's runs. The groups must be exactly those the
     parameters prepared for every critical case, and the authority's registry digest must
     match. A failure refuses a full plan, and a partial plan falls back to the control.
   - **Calibrated scope.** A full plan whose environment or recipe the cohort did not calibrate
     is unavailable (`uncalibrated_scope`). An unprepared group is listed as uncalibrated and
     never as calibrated.
   - **Fetched verification.** The activation change itself is verified with `--fetch`. Trusted
     control code fetches the history down to the anchor, the originals' attempt and job
     records, every cohort-named run's artifact listing and the originals' bundles, with a
     read-only token. It recomputes readiness, parameters included. Members, classifications,
     doses, tallies, groups and the decision must equal the record; history size and later
     refusals may differ. Missing or expired evidence makes a member missing and refuses. The
     archive is a retention backup, not a promotion source.
8. **An equivalent adapter changes only its adapter** (`_equivalence`, `protected_diff`).
   - **The binding.** The manifest names the reviewed calibrated manifest, retained in the
     control revision with its SHA-256, and keeps all of its records, runs and scope.
   - **The diff.** Between the two authorities, the harness paths may differ only in the named
     adapter files, `adapters.toml` entries of the named equivalence group, and calibration
     records. The named files must lie in that group's closures, and `suite.toml` must be
     byte-identical.
   - **Still owed.** Owner semantic approval, golden checks on both sources and a post-merge A/A
     are listed as `pending`. They are never established by the check.
9. **Cost and shipping demonstrations are computed, not claimed** (`perflab.costs`).
   - **Cost.** `cost-report` takes retained PR and same-source runs with their declared cold or
     warm cache. It records each run's build time, per-shard sampling time, per-case samples
     and wall time, artifact bytes and failures, then each group's median and maximum.
   - **Shipping.** `shipping-report` takes every measured candidate sample of each budget's
     instances on its named environments. Each must be strictly below its limit, and there must
     be at least its minimum count.
   - **Status.** Both readers recompute every aggregate. Either is `demonstrated` only from
     measured evidence that covers everything; otherwise it is `pending`, or `missed`.

## Consequences

- **Tests.**
  - **`test_calibration`.** A shared synthetic cohort over a real Git authority covers every
    repair on the predecessor's counterexamples:
    - two deleted failed originals beside successful replacements refuse, with `total_count`
      matching the remaining runs;
    - a deleted newest original stays missing until any later run exposes it;
    - failed, cancelled, timed-out, skipped and null attempts are `failed`;
    - failed A/A and preparation originals supply nothing;
    - A0-200 parameters are refused while the originals give 100;
    - duplicate run and shard artifacts in both orders are `artifact_ambiguous`;
    - six runner forgeries and a missing job record are `runner_unverified`;
    - impossible dates exit 2 without a traceback.
  - **`test_activation`.** Reviewed and fetched verification of a ready cohort agree, against a
    stand-in API that answers like the documented endpoints. Refused are:
    - a self-consistent readiness claiming valid doses the samples contradict;
    - expired and deleted members;
    - altered record hashes, another environment, recipe, group, run set, cohort, policy or
      authority;
    - a bridge changing the registry, harness, another adapter group or files outside the
      group's closure.

    An unprepared group is uncalibrated.
  - **`test_planning`, `test_ci`, `test_costs`.** An active plan rehashes its records and
    refuses an uncalibrated tier. `ci-schedule` refuses dedicated calibration. Cost and shipping
    aggregates, strict limits and pending states are covered.
  - **`perf_probe`.** The 1 ms setup-lease test waits through a setup hook until the Store
    reports the lease lapsed, instead of assuming it.
- **Pending operational evidence.** Nothing here dispatched a workflow or read an authenticated
  API. No cohort, activation, PR or same-source cost run, or shipping sample has run on real
  hosts. The runner-name and runner-group evidence follows GitHub's documented job fields and
  observed hosted naming. A five-day hosted cohort, fetched activation of it, cost and shipping
  demonstrations, staging handoff and ruleset coexistence remain operational evidence of p8.
- **Not changed.** The 15-second lease, the 8-MiB envelope cap, the whole-trial readiness rule,
  fixed denominators, release handoff and deduplication, advisory enforcement, and the native
  calibration builds.
