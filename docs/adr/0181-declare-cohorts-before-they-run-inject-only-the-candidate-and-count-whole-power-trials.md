# ADR-0181: Declare cohorts before they run, inject only the candidate, and count whole power trials

**Status:** accepted (2026-10-20). Implements the calibration part of package p6b of the
[continuous performance laboratory](../design/continuous-performance.md) (design §15). It first
closes the release-history finding and the two gate failures left on
[ADR-0180](0180-hand-off-published-releases-read-only-and-measure-each-claimed-identity-once.md)'s
source. Activation verification is a later p6b Task. ADR-0177 to ADR-0180 stand except where
this ADR amends them.

| Contract | Version | What changed |
| --- | --- | --- |
| Run plan | `af.perf-run/5` | a calibration member's `cohort.injection`; `/4` is refused |
| Cohort | `af.perf-cohort/2` | members carry a day and no Actions run ID; schedule start, phase order and the preparation count; `/1` is refused |
| Calibration parameters | `af.perf-calibration-parameters/2` | A0 and loop-cost provenance per case and group, nullable when unavailable; `/1` is refused |
| Readiness | `af.perf-readiness/2` | per-member provenance, status reasons, per-case doses, whole-trial tallies, group readiness and refused evidence; `/1` is refused |

## Context

1. **Unreadable trusted claims were taken as absence** (`p6b-release-root-unreadable-trusted-history`).
   `release.read_history` gave a reason to an artifact of the claim's exact name and ignored it.
   That included artifacts from this repository's handoff workflow on `main` that were
   unreadable, oversized or of another identity. Such an artifact may be an earlier claim, so
   the handoff claimed again and confirmation measured.
2. **The authority sample's lease could lapse during setup** (`p6b-release-gate-authority-setup-lease`).
   `authority::build` opened the 15-second lease and only then captured the host and authority,
   which validates the compiled plan. `authority_sample` then captured a second time before its
   pre-region renewal. Moving the capture out was not enough. Timing the steps on this host
   (load average about 100 on 14 logical CPUs) put compiling at 3-6 s and capturing under
   1 s. Proposing took 3-4 s and admitting 9-10 s, so open to admission took 12-15 s. A
   renewal after admission then found the lease expired, reproduced twice. Those two writes
   need the lease and cannot leave it.
3. **A group kill can miss a newborn process** (`p6b-release-gate-inherited-pipes`). The fixture
   `(sleep 1; touch survived) & exit 0` failed about one run in four on this macOS host. A
   direct reproduction (`killpg` on the exited leader's group, then `ps` 300 ms later) found
   `sleep 1` alive in 27 of 100 runs, still in the group. The subshell was forking it as the
   one `SIGKILL` arrived. macOS signals the parent but not the new child, which holds the
   inherited pipes until it exits. The supervisor's uncertainty was real: a process it meant to
   kill was alive.
4. **Calibration was declared but not executable.** `af.perf-cohort/1` carried a nullable
   `actions_run_id` per member, which could only be filled in after the run and so would have
   changed the frozen cohort's digest. Nothing injected work, sealed A0 or the loop counts,
   bound a member to its retained evidence, or counted power trials as whole trials.

## Considered options

1. **Increase the cleanup grace, retry, or accept uncertainty for the fixture.** Rejected: a
   longer grace hides a real survivor, and the survivor in the reproduction outlives any
   reasonable grace.
2. **Renew the lease periodically during setup, or extend it.** Rejected: a heartbeat outside
   production semantics hides genuine expiry, and the lease stays 15 s.
3. **Write Actions run IDs into the cohort after each member runs.** Rejected: the cohort must
   be fixed before execution, so its bytes and digest never change.
4. **Infer a member's attempt from the run's latest `run_attempt`.** Rejected: the artifact API
   has no attempt, and a rerun moves the latest attempt.
5. **Inject by sleeping, by a wrapper process, or by a candidate-only build flag.** Rejected by
   the design: the loop is real CPU work inside the timed operation, in one build of one source
   on both sides.
6. **Count per-case detections.** Rejected: one case failing trial 1 and another failing trial 2
   gives each case 9 of 10, but only 8 whole trials qualify.

## Decision

1. **Trusted unreadable evidence is unavailable history** (`release.read_history`). Producers are
   still checked first. An artifact from another repository, branch or workflow stays
   `foreign_run` or `foreign_workflow` and is ignored. One from this repository's
   `performance-release.yml` on `main` that is oversized, unreadable or of another identity
   (key, identity, kind or claimant) is retained with its reason. The whole history is then
   `unavailable`: the handoff claims nothing and confirmation measures nothing.
2. **The region gets a lease of its own** (`authority::build_then`, `hand_over`,
   `take_fresh_lease`).
   - **One capture.** The host and authority are captured once, before `open_task`. The setup
     writer's lease spans only open, propose and admit.
   - **Ending the setup lease.** After admission it is released, through the public writer. If
     the Store reports it already lapsed after that last successful write, nothing is written
     for it. Any other failure is the fixture's.
   - **A fresh lease.** The region's writer then takes a fresh production lease at the next
     epoch (`take_task_lease`), as the last Store write before the region, and opens its fresh
     Store connection.
   - **What still fails.** A write that runs past its lease still fails in the Store, and a
     region longer than 15 s still ends `lease_expired`. Nothing is retried or extended, and
     no heartbeat runs in a region.
   - **Unchanged paths.** The plain fixture's log stays exactly opened, proposed and admitted.
     The expiry tests keep the setup lease and let it lapse at the boundary.
3. **A killed group is signalled again until it settles** (`review_process::sample`). The leader
   is unreaped, so the group's ID names only the sample. While the drains have not ended, or a
   member other than the zombie leader still accepts a signal, the group gets `SIGKILL` again
   every 5 ms. Linux enumerates the group's live members. macOS answers `EPERM` for a group of
   zombies and `ESRCH` for an empty one. One cleanup grace bounds settling, verification and
   draining together. A pipe still held or a member still live after it stays
   `cleanup_uncertain`.
4. **A default-off loop** (`review_core::perf_calibration`, feature `perf-calibration` of
   `review-core`, `review-pipeline` and `af`).
   - **The loop.** `AF_PERF_CALIBRATION_ITERATIONS` names a canonical count of at most 2^40. The
     probe runs the loop inside each timed region, after the operation; `af` runs it first
     inside its own process. It is a linear-congruential loop through `black_box`. It reads and
     writes no state, so every oracle sees its usual result.
   - **The report.** One stderr line, `af.perf-calibration-injection/1 iterations=N
     injected_ns=M`, gives its measured duration.
   - **Other builds.** A build without the feature refuses any value of the variable (exit 2)
     and runs no loop. `make lint` compiles the probe and `af` with the feature, and its
     feature-on core tests run.
5. **Members are frozen logical identities** (`perflab.calibration`).
   - **The cohort.** `cohort-declare` declares the cohort from the authority's own registry:
     20 A/A members on days 0-1, the preparation members on day 2, then 30 descriptive and
     10 power trials, over at least five days. Members carry seeds, no Actions run IDs, and the
     allowed CPU model and image groups.
   - **The plans.** `plan --scope calibration` freezes `cohort.injection` (`af.perf-run/5`).
     An A/A member has none. A preparation member gets the cohort's declared count for every
     case in every group. A slowdown member gets the sealed count for its level, case and
     group, and the digest of the parameters it uses. Only prepared groups are listed.
   - **Running them.** Both sides build with `perf-calibration`. The runner refuses a shard
     whose host CPU and sample image are no listed group (`unprepared_cpu_group`). Only the
     candidate's warmup and measured samples get the count; the baseline and sentinels get `0`.
     Each case sample records its reported dose as `calibration_injected_ms`, or why it is
     unavailable.
   - **No budget.** An injected plan enforces no budget, and any other scope refuses the
     controls.
6. **Sealed parameters** (`calibration-prepare`).
   - **A0.** For each case and allowed group, A0 is the median baseline of every measured A/A
     sample there. Zero or none leaves it unavailable with a reason, and that group is not
     ready.
   - **The loop's cost.** It is the median preparation-candidate dose per iteration.
   - **The counts.** Each level's count is the nearest whole count, ties up, to its target:
     2%, 5% or 10% of A0, or `p = max(10%, 2·max(r, a/A0))`. Readers recompute every target
     and count exactly. Trials name the sealed digest, so they cannot precede or retune it.
7. **Readiness from retained evidence** (`readiness`).
   - **The inputs.** It takes an Actions artifact listing, which has no `run_attempt`, and the
     downloaded `af-perf-*` artifacts. A member's run is bound by the lab run ID frozen in its
     plan, `gh<run>_a<attempt>_primary`, and its artifact names.
   - **The original.** It is the member's first Actions run at attempt 1. A later attempt, another
     dispatch of the member, an unknown member, another cohort, a duplicate artifact or an
     unbound run is refused and recorded, never counted.
   - **Judging a run.** Verdicts are recomputed from the samples. Statuses are missing,
     interrupted, failed, unhealthy and invalid, the last for off-schedule, wrong plan,
     before-calibration, CPU-group mismatch, unprepared group or unmeasured evidence. All of
     them stay in every denominator.
   - **Doses.** A dose is valid when the median candidate dose of the case's N measured pairs
     lies within 75% and 125% of its target, inclusive. Validity never uses the observed
     slowdown.
   - **The decision.** Ready means all 20 A/A members within tolerance in every critical case,
     plus at least 9 of 10 power trials whose every critical case is a regression with valid
     dose in that same trial. Per-case and per-group tallies are descriptive, and the document
     says that 9 of 10 is no population power claim. The command exits 0 only when ready.

## Consequences

- **Tests.**
  - **`test_release`.** The foreign-producer controls still measure. A not-ZIP artifact, an
    invalid typed claim, an oversized artifact, a claim of another identity and another
    claimant are each `history_unavailable`, with the evidence retained and nothing measured.
    The real sequence of handoff, claim upload and confirmation is blocked by an earlier
    unreadable trusted artifact.
  - **Lease.** `perf_probe` puts a delay where the capture's cost lies. It checks that `opened`
    follows the delay, and that the log is opened, proposed, admitted, released, then a 15-s
    lease taken at epoch 2. A failing step there opens no lease. A short setup lease, waited
    out until the Store reports it lapsed, is left lapsed and still refuses renewal, and a fresh
    lease is taken. Both package sizes then passed their oracle on this loaded host, where
    they had failed twice before.
  - **Settling.** The new `sample` regression fails without settling and passes with it. The
    reproduction script went from 24 of 40 uncertain runs to none.
  - **Refusing the loop.** `cli_surface` and `perf_probe` check that a verdict build refuses
    the control.
  - **`test_calibration`.** It runs a complete synthetic cohort end to end, generated at test
    time. It covers whole-trial intersection, A/A boundaries, missing, unhealthy, interrupted,
    off-schedule and misplaced members, reruns and replacements, inclusive dose limits,
    candidate-only injection, forged readiness and unchanged cohort bytes.
- **Pending empirical evidence.** Nothing here ran on a hosted runner or over five days. Every
  cohort number in the tests is synthetic. A real cohort, its multi-day schedule, measured A0
  and loop costs, and shipping-budget demonstrations remain operational evidence. Activation
  verification, which fetches the cohort's artifacts with a token, is the next p6b Task.
- **Not changed.** The 15-second lease, the 8-MiB envelope cap, verdict statistics, health
  rules, the workflow's rerun guard (a member's lab run ID still carries its run and attempt),
  advisory enforcement and the release handoff's other rules.
