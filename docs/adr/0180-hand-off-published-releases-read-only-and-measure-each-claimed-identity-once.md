# ADR-0180: Hand off published releases read-only, and measure each claimed identity once

**Status:** accepted (2026-10-20). Implements the release part of package p6b of the
[continuous performance laboratory](../design/continuous-performance.md). Calibration,
readiness and activation verification are later p6b Tasks. It first closes the trace-cap race
left on
[ADR-0179](0179-link-only-sanitized-views-and-bind-a-diagnostic-child-to-its-parent-workloads-and-frozen-recipe-kind.md)'s
source. ADR-0177, ADR-0178 and ADR-0179 stand except where this ADR amends them.

| Contract | Version | What changed |
| --- | --- | --- |
| Schedule | `af.perf-schedule/2` | adds `release` and trusts `workflow_run`; `/1` is refused |
| Release claim | `af.perf-release-claim/1` | new |
| Release handoff | `af.perf-release-handoff/1` | new |

## Context

1. **A trace's reason depended on scheduling** (`p6a-integrity-root-trace-cap-reason`).
   `Profile.cli_run` raised `cli_failed` from `supervision_failure` before `exported_trace`
   settled the evidence. The watch counts a staged CLI trace while it is written. When it
   cancelled a live writer that had exported 8,192 bytes into 2,048 of room, the omission said
   `cli_failed`. The same bytes from a writer that finished before the watch looked said
   `trace_over_cap`. The mandatory gate's `TraceRoom` assertion failed on that race. Resource
   enforcement and the retained, counted bytes were right on both paths.
2. **Releases were not measured.** Design §12 asks for a read-only downstream handoff of
   completed upstream `Release` runs. It must verify the publication, deduplicate by the
   immutable upstream run, tag and SHA, and claim before sampling. A later pin-only retry must
   link the retained result, and a claim without a result must never be measured again
   automatically. `ci.schedule` skipped every `workflow_run`.

## Considered options

1. **Accept either reason for an over-room trace, or delay the watch until exit.** Rejected:
   either one hides the race, and the second also lets a live writer outgrow its room.
2. **Run the handoff inside `performance.yml` with an Actions token.** Rejected: that workflow
   is token-free by contract, and its candidate-building jobs must stay so.
3. **Trust the `workflow_run` payload, its workflow name or its overall conclusion.**
   Rejected: any workflow can be named `Release`, and a failed pin retry makes a published
   release's run fail.
4. **Read only the latest attempt's jobs.** Rejected: a pin-only retry hides the attempt that
   published.
5. **Deduplicate in a cache, an issue or a branch.** Rejected: each needs write access.
   Artifacts are the run's own immutable, listable record.
6. **Let a duplicate measure when the history is unreadable or expired.** Rejected: the design
   runs nothing then.
7. **Download the upstream release's artifacts.** Rejected: upstream bytes would become
   workflow input. The tag is built from Git.

## Decision

1. **Confirmed room exhaustion is `trace_over_cap`** (`Profile.settled_trace`). A traced CLI
   execution whose supervisor confirmed its end is settled on the bytes the evidence holds now,
   before any supervision failure is classified. It does not matter whether the watch cancelled
   the command or it had finished. An unconfirmed end still keeps its scratch, and
   `cleanup_uncertain` wins. Under the room, a CLI failure stays `cli_failed`. Accounting,
   oracles, deadlines, live cancellation and next-side suppression are unchanged.
2. **A read-only handoff** (`.github/workflows/performance-release.yml`, `perflab/release.py`).
   - **The token.** Its `handoff` job alone has a token: the run's own `github.token`, with
     `contents: read` and `actions: read`, passed to two steps. The job runs only from `main`,
     checks out with `persist-credentials: false`, and downloads only claim artifacts, as data.
   - **The measurement.** The `measure` job calls `performance.yml` (`workflow_call`) with
     `contents: read`, no token and only the claim's artifact name.
3. **Eligibility** (`release.verify`).
   - **Workflow and repository.** The run's workflow ID must be the one the API gives the path
     `.github/workflows/release.yml`. The event, the API run and every job must agree on the
     repository, head repository, run ID, attempt and head SHA; disagreement is `refused`.
   - **Event and tag.** It must be a completed push. A `main` push needs exactly one release
     tag on its commit, and without one it is `not_eligible`; a pushed `vX.Y.Z[-rc.N]` tag is
     its own release. The tag must name the head SHA in the checkout and through the API.
   - **Release.** It must be published, not a draft, with the tag's prerelease flag.
   - **Publication.** The `sign and publish` job must have succeeded in some attempt. Up to 20
     attempts are read, each through at most three complete pages. A total that is exceeded
     or changes while it is read is `incomplete`. A failed pin in any attempt does not matter.
4. **Claim once, before any sample.**
   - **Identity.** The upstream run ID, tag and SHA, never the attempt. Its key is the SHA-256
     of its domain-separated canonical serialization. The claim `af.perf-release-claim/1` is
     uploaded as `af-release-claim-<key>`, never an `af-perf-*` transport name, and kept 90
     days.
   - **Valid claims.** A claim is valid only when this repository's `performance-release.yml`
     wrote it from `main` and its bytes hold exactly this identity and kind. Other artifacts
     of that name are recorded with their reason and ignored.
   - **Duplicates.** With a valid claim present, a run claims nothing. It is `linked` to that
     claim's retained `af-perf-<prefix>*-run` bundles, or `incomplete` as
     `claim_without_result` or `result_expired`.
   - **Unreadable history.** An unreadable or bounded listing (more than 20 claims), an expired
     claim, or an upstream run older than 89 days runs nothing.
   - **Confirmation.** After the upload the history is read again. Only the first valid claim,
     by creation time and artifact ID, is `measure`. A loser of a race links or reports the
     winner, and an unlisted own claim is `claim_unconfirmed`. Every decision is
     `af.perf-release-handoff/1`, retained whatever its outcome, and always advisory.
5. **Explicit remeasurement.** A dispatch names `release_run` and `remeasure_of`, the parent
   claim's key. The same identity is verified, and its parent must be a valid claim. It writes
   its own `af-release-remeasure-<key>-<run>` claim naming the parent. It plans a manual
   comparison of the same two commits with `remeasure_of` set to the parent's primary run ID.
   The parent claim is never changed.
6. **Scheduling a claim** (`ci.schedule(..., release=claim)`, `af.perf-schedule/2`).
   - **The call.** The called plan job runs the scheduler from the handoff's default-branch
     SHA. The claim must be this run's own. Its tag must still name the claimed SHA, or the
     comparison is `tag_moved`.
   - **The plan.** A release plans the release tier under the existing rule: the highest
     smaller stable tag, and a prerelease predecessor only for a prerelease. Without an
     activation it is `no_active_authority`, with the advisory control bootstrap.
   - **The schedule.** It records the claim. `workflow_run` is a trusted event, and one
     without a claim is skipped. Each release measurement has its own concurrency group.
   - **The harness.** `performance-release.yml` is a harness path.

## Consequences

- **Tests.**
  - **`TraceRoom`.** A bounded live writer stays 3 s after exporting. It shows the watch's
    confirmed cancellation, no finished marker and `trace_over_cap` with the exact bytes. It
    fails without the repair. A completed-oversize control has the watch's first look patched
    to after exit and gets the same reason and account. The valid-small and under-room
    `cli_failed` controls stand beside it.
  - **`test_release`.** A stand-in API covers same-name wrong workflows, wrong repositories and
    forks, and contradicted identities. It covers paginated and bounded attempts,
    publication with a failed pin retry, and the first valid claim beside foreign ones. It
    covers a duplicate race, an interrupted claim, a pin-only link, expired and unavailable
    history, and a remeasurement that keeps its parent. It also schedules claims.
  - **Workflows.** `test_release_workflow` and `test_performance_workflow` check the workflows'
    text and run the branch and control guards.
- **Pending operational evidence.** No hosted handoff has run. The staging demonstration in
  `docs/performance-ci.md` (publication with a failed pin, duplicates, an interrupted claim and
  a remeasurement) stays pending until an authorized run. So does verifying that rulesets
  coexist with the release pin's push before any required check.
- **Not changed.** `release.yml`, the 15-second lease, the 8-MiB envelope cap, every verdict and
  diagnostic bound, the collector's `af-perf-*` transport format, and advisory enforcement.
  Calibration, readiness and activation verification, cost tooling and guarded kernel
  experiments remain later Tasks.
