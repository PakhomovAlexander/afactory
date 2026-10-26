# Continuous performance runs: operator runbook

`.github/workflows/performance.yml` runs the performance laboratory for pull requests to `main`,
pushes to `main`, a nightly schedule and explicit dispatch
([ADR-0177](adr/0177-schedule-continuous-runs-from-trusted-control-and-move-builds-once-to-bounded-shards.md),
[design §12](design/continuous-performance.md#12-ci-authority-releases-and-exceptions)).
`.github/workflows/performance-release.yml` hands each published release to it
([ADR-0180](adr/0180-hand-off-published-releases-read-only-and-measure-each-claimed-identity-once.md)).
A pull request labeled exactly `no-perf` is not measured; its `Performance check` reports the
exemption
([ADR-0196](adr/0196-skip-pull-request-measurements-only-for-an-explicit-no-perf-label.md),
[when to apply it](performance-sdlc.md#the-no-perf-label)).
The initial rollout is advisory, as reviewed in `fixtures/performance/enforcement.toml`.
The stable `Performance check` can enforce a qualified full primary only after activation;
the workflows never edit a ruleset or branch protection. See [SDLC integration](performance-sdlc.md)
for image provisioning, continuous collection and the rollout sequence. This runbook says what
the operator provides, what each job does, and what has not been demonstrated.

## What has not been demonstrated

Hosted partial A/A run [39633877665](https://github.com/PakhomovAlexander/afactory/actions/runs/39633877665)
built the GNU products and verified the 2-GiB scratch on Ubuntu. It retained all 216 slots,
including 28 setup failures caused by a detached release update check, and aggregation failed
on the artifact action's flat single-download layout. These original failures are retained.
[ADR-0192](adr/0192-admit-single-downloads-and-run-cli-measurements-offline.md) repairs those
boundaries; a new hosted run must verify the repaired path and collection before calibration.
Fixture tests alone do not establish hosted readiness, performance or collection before expiry.

No calibration cohort has run either. Its declaration, preparation, injection and readiness
rules were checked against a synthetic cohort generated at test time
(`tests/test_calibration.py`); that is not empirical readiness, and nothing may be activated
from it. Activation verification was checked against the same synthetic cohort through a
stand-in API (`tests/test_activation.py`), and the cost and shipping demonstrations against
synthetic runs (`tests/test_costs.py`); none of them is an activation, a cost or a speed
measurement. The activation workflow (`performance-activation.yml`) passes actionlint 1.7.12,
and its trust selection was exercised by executing its own steps against a disposable
repository (`tests/test_performance_workflow.py`); it has not run on GitHub.

No release handoff has run either. Its eligibility, claim and deduplication decisions were
checked against a stand-in API (`tests/test_release.py`) and its workflow as text; that is not a
hosted handoff. The staging demonstration below stays pending until an authorized run in a
staging repository shows it.

## What the operator provides

Repository variables, never secrets; the workflow reads no secret.

| Variable | Meaning | When it is unset |
| --- | --- | --- |
| `AF_PERF_GNU_BUILD_IMAGE` | The quick recipe's build and sample image: a registry reference pinned by digest, `NAME@sha256:...`, whose platform and Rust 1.88.0 toolchain the operator verified for the hosted runner. | Every quick comparison is unavailable as `no_build_image`. |
| `AF_PERF_MUSL_BUILD_IMAGE` | The nightly and release shipping recipe's image: Rust 1.88.0, cargo-zigbuild 0.23.4 and Zig 0.15.2, pinned by digest. | The nightly and release comparisons are unavailable as `no_build_image`. |
| `AF_PERF_EXECUTOR` | `dedicated` lets trusted events (push, schedule) use the dedicated runner; a calibration member is refused rather than run on it. | Every job runs on ephemeral hosted runners. |
| `AF_PERF_DEDICATED_RUNNER` | The dedicated runner's label. | Dedicated capacity is unavailable; jobs run on hosted runners. |

A local image ID (`sha256:...` alone) is refused: a hosted runner cannot pull it. Before setting
an image, verify its manifest for the runner's platform and its tools' versions with a bounded
registry and tool observation. The build checks the toolchain inside the image again before any
compile, and the plan freezes the reference, so a later change of the variable affects only
later plans.

Pull requests are never trusted, whether or not they come from a fork: they run only on
ephemeral hosted runners. A pull request from a fork may not receive repository variables, and
its comparisons are then unavailable rather than built on the host.

## Administrative prerequisite: owner review at the exact head

The candidate's workflow file and harness are candidate-controlled in a pull request. The
laboratory measures with the control revision's activated authority and labels any harness
change `harness_changed`, but a hostile author could still change what the pull request's own
workflow does. The defense is administrative, outside this workflow:

- `.github/CODEOWNERS` names the owner for every path, the authority and workflow paths
  included.
- The `main` ruleset requires code-owner review, dismisses stale approvals when new commits are
  pushed, and requires approval of the most recent reviewable push, so an approval always
  covers the exact head.

This is not cryptographic enforcement, and the laboratory does not set or check these settings.
Export the ruleset before and after any change to them, as the design requires before a
required check is enabled.

## The hosted sample scratch

Samples need a verified 2-GiB disk-bounded scratch filesystem. On a standard hosted Linux VM the
shard job prepares one in a trusted step before any candidate byte runs:

```sh
python3 "$RUNNER_TEMP/control/scripts/performance/cli.py" ci-scratch \
  --image "$RUNNER_TEMP/scratch.img" --mount "$RUNNER_TEMP/scratch-fs"
```

It creates a 2-GiB ext4 image, mounts it through a loop device with `nodev,nosuid` using the
runner's passwordless sudo, and gives it to the runner's user. It refuses, creating nothing,
when the host is not Linux, when sudo needs a password, or when free disk would fall below the
10-GiB floor plus the image. The shard's own preflight then verifies the bound by an enforcement
fill. No container receives a capability or privilege, and nothing changes a kernel setting.
`ci-scratch --cleanup` unmounts and removes it. The single-CPU `ubuntu-slim` runner cannot mount
filesystems and is not a supported profile. If the measured free disk is too small, use another
declared profile; the floor is never lowered.

## Jobs

Pull requests run on `opened`, `synchronize`, `reopened`, `labeled` and `unlabeled`, in one
concurrency group per pull request, so every label change replaces the running run.

1. **scope.** Checks nothing out. Outputs `exempt=true` only for a `pull_request` event on run
   attempt 1 whose pull request carries a label named exactly `no-perf`; every other trigger, a
   release claim and every rerun measure. Unreadable labels fail it. An exempt run plans, builds
   and samples nothing.
2. **plan.** Runs only when `scope` succeeded and is not exempt. A calibration dispatch first
   refuses an absent, malformed or stale `dispatch_head` before checkout or planning. Checks out
   the event with `persist-credentials: false`, resolves the trusted control
   revision (the PR's base SHA, the pushed commit's first parent, the scheduled tip, or the
   dispatch's control or base), and runs `ci-schedule` from that control's worktree. A
   dispatch's `refs/heads/NAME` is read as the checkout fetched it, `refs/remotes/origin/NAME`,
   and refused as ambiguous when a local branch of that name names another commit; a full SHA
   or `refs/tags/` ref resolves as given. A release handoff's call passes only its confirmed
   claim; the control is then the handoff's default-branch SHA, and the claim names what is
   measured. The
   schedule (`af.perf-schedule/2`) and the frozen plans are uploaded as
   `perf-plan-<actions run>-<attempt>`. The job summary lists every comparison and why any is
   unavailable.
3. **build** (one per planned run). Pulls the frozen build image, refuses any credential in
   `.git/config` or the environment, builds once with `cli.py build`, packs the build with
   `ci-pack`, scans the pack for credentials and uploads it as `perf-build-<run>`. A failed
   build's `builds.json` and logs go to `perf-build-failure-<run>`.
4. **shard** (one per planned shard, at most the plan's concurrent jobs at once). Refuses a job
   rerun of an earlier attempt's plan, verifies and unpacks the build with `ci-unpack`, prepares
   the scratch, checks credentials, runs exactly its shard, and stages its evidence, failed or
   not, as `af-perf-<run>-shard-<i>`.
5. **aggregate** (always, one per planned run). Downloads shard artifacts into named directories
   (one match is flat in download-artifact v8), imports them with `ci-aggregate`, recomputes the verdict, writes the report and the
   job summary from the validated report, and uploads the run bundle as `af-perf-<run>-run`
   with the report and summary as views, and the report directory as `perf-report-<run>`.
6. **Performance check** (always). For an exempt run it only reports `exempt (label no-perf),
   not measured` and succeeds; that is no measurement and no qualification. Otherwise it reads
   the trusted control's enforcement policy and checks the
   exact scheduled primary's report and all upstream job results. A missing build or report,
   failed or cancelled job, partial scope, synthetic result or inconclusive verdict does not
   qualify. Advisory mode shows that outcome without blocking. Required mode fails it on PRs
   and main pushes; dispatches, calibration, nightly and post-publication release runs remain
   advisory even after PR enforcement is enabled.
7. **Performance report — RESULT (scope)** (always). The PR check list names the actual
   verdict: `PASS`, `FAIL`, `INCOMPLETE` or `UNAVAILABLE`. Its Details summary shows every
   scheduled comparison, coverage and a case table with rounded decimal medians and percentage
   changes. Bootstrap success is explicitly scoped: `PASS (bootstrap; primary unavailable)`
   never means a qualified full primary. Synthetic evidence is labeled, a missing report is
   unavailable, and failures take precedence over passing comparisons. The stable check also
   emits notice/warning annotations and the same summary. Advisory checks can remain green
   when the displayed verdict is `FAIL`; qualification and enforcement policy are unchanged.
   A valid `no-perf` exemption instead names the check `EXEMPT (no-perf; not measured)` and
   states that no performance claim is made. Failed or cancelled scope decisions stay
   `UNAVAILABLE (scope decision failed)`, even if the scope emitted an exemption before it
   stopped. The original scope guard and the measured path's control checks still apply.

From a PR, open **Performance report → Details**, then **Run and report artifacts**.
Download the named `perf-report-<run>` artifact and open `report.html` for the interactive
report; `verdict.json` preserves exact values. The compact summary shows at most 100 cases per
comparison, with failed/incomplete cases first and an explicit count of omitted rows.
All presentation is rendered from validated, plan-bound verdict JSON by trusted control code;
downloaded Markdown and HTML are never copied into GitHub summaries. The report job uses only
status outputs and needs no write token or candidate checkout.

The integration must first merge into the trusted control revision. The PR that installs it
still runs the base's older status script, so its new check can say
`UNAVAILABLE (trusted control has no report output)` and link to existing run summaries and
artifacts. Later runs using the merged control publish the verdict. A status job interrupted
before producing outputs also shows this explicit fallback.

Pull request artifacts are retained 14 days, all others 90. Only `af-perf-*` artifacts are numeric
evidence in the collector's `af.perf-transport/1` format; the plan, build and report artifacts use
other names, so `cli.py archive --fetch` never mistakes them for evidence.

## Reruns

A rerun never inherits a `no-perf` exemption: it reads the event as it was when the run began,
so `scope` measures on every attempt after the first. A shard runs once. Rerunning failed jobs reuses the earlier attempt's plan, so its shard jobs
refuse to run and the original evidence stays. Rerun the whole workflow instead: its plan job
freezes new runs whose IDs carry the new attempt. Both attempts' artifacts remain.

One Actions run can hold several laboratory runs: a rerun's attempts, or a primary and its
bootstrap comparisons. The collector refuses to mix them, so collect each with
`cli.py archive --fetch ACTIONS_RUN_ID --lab-run RUN_ID`, which takes only that run's
`af-perf-<run>-*` artifacts.

## Release handoff

`performance-release.yml` runs when a run of the repository's `Release` workflow completes, and
on an explicit remeasurement dispatch. Its `handoff` job reads; it never writes anything but its
own artifacts:

1. **Refuse anything but `main`.** The job runs only from `refs/heads/main`, the reviewed
   workflow; `workflow_run` always runs the default branch's file.
2. **Verify the upstream release** (`ci-release-handoff`). The run must be of this repository's
   `.github/workflows/release.yml`, bound by the workflow ID the API gives that path, never by a
   display name. It must be a completed push to `main` whose commit carries one release tag, or
   a pushed `vX.Y.Z[-rc.N]` tag. The tag must name the run's head SHA in the checkout and
   through the API, and its release must be published, not a draft, with the tag's prerelease
   flag. The `sign and publish` job must have succeeded in some attempt. Every attempt's jobs
   are read completely through bounded pagination, so a later failed `pin` retry never hides
   an earlier publication. A main push without a release tag is `not_eligible`. A listing
   that hits its bound or changes while it is read is `incomplete`.
3. **Read the claim history.** The identity is the upstream run ID, tag and SHA, never the
   attempt; its key is the SHA-256 of its canonical serialization (`af.perf-release-claim/1`).
   Valid claims are artifacts named `af-release-claim-<key>` that this repository's
   `performance-release.yml` wrote from `main` with exactly this identity. Other artifacts of
   that name are recorded and ignored. When a valid claim exists, the run links its retained
   `af-perf-<run>-run` bundles (`linked`), or reports `claim_without_result` and measures
   nothing. When the history cannot be read completely, or may have expired, nothing runs.
4. **Write the claim before any sample.** The claim is uploaded and kept 90 days.
5. **Confirm it** (`ci-release-confirm`). The history is read again, and only the first valid
   claim, ordered by creation time and then artifact ID, measures. A run that lost a duplicate
   race links the first claim's result or reports it incomplete.
6. **Retain the decision.** `handoff.json` and `confirm.json` (`af.perf-release-handoff/1`)
   are uploaded as `perf-release-handoff-<run>-<attempt>`, whatever the outcome. Incomplete and
   refused decisions fail the job visibly; they never affect the release.

Then the `measure` job calls `performance.yml` with the claim's artifact name alone. It plans
the release tier: the published tag against the highest smaller stable release, or a
prerelease's highest smaller predecessor. It builds both from Git and runs them in the
unchanged build, shard and aggregate jobs. The measurement is advisory and never gates
publication.

### Credentials

| Job | Token | Permissions | What it reads or runs |
| --- | --- | --- | --- |
| `handoff` | this run's own `github.token`, passed only to its two API steps as `AF_PERF_RELEASE_TOKEN` | `contents: read`, `actions: read` | The upstream run, its attempts' jobs, the tag, the release and the claim history; it downloads only claim artifacts, as data |
| `measure` (`performance.yml`) | none | `contents: read` | The published tag's source, built credential-free |

No job receives a publishing credential, the release signing key or a write permission. Nothing
downloads or executes an upstream artifact; the release's binaries are never workflow code
here. A pull request never triggers the handoff.

### Explicit remeasurement

Dispatch `performance-release.yml` from `main` with `release_run`, the upstream Release run ID,
and `remeasure_of`, the parent claim's key (`sha256:...` from its `handoff.json`). The handoff
verifies the same identity and its parent claim. It then writes its own
`af-release-remeasure-<key>-<run>` claim and measures anew, as a manual comparison of the same
two commits naming the parent's run. The parent claim is never changed or replaced. A claim
without a result is never remeasured automatically; this dispatch is the only recovery.

### Staging demonstration (pending)

Before relying on the handoff, demonstrate it in a staging repository that has the same
workflows, without giving any AF Task a publishing credential:

1. In the staging repository, a human operator holds the release signing secret. Cut a release
   whose `sign and publish` succeeds. Confirm the handoff claims it and its `measure` job runs.
2. Make the `pin` job fail, for example by protecting `main` against its push, and rerun only
   the failed job. Confirm the second handoff links the first claim's result or reports it
   incomplete. It must not measure again.
3. Let a second handoff of one identity run while the first still measures, for example by
   rerunning the failed `pin` job meanwhile. Confirm only the first valid claim measures.
4. Cancel a handoff after its claim is written and confirm later runs report
   `claim_without_result` without measuring. Then dispatch an explicit remeasurement and
   confirm it measures and keeps the parent claim.
5. Push to `main` without a release and confirm `not_eligible`.

Keep each run's `perf-release-handoff-*` artifacts and job summaries as the evidence. Before
`Performance check` becomes required, export the rulesets and verify they coexist with
the release pin's authorized push (design §12). If they do not, stay advisory and propose a
separate release and ruleset decision.

## Calibration cohort

A proposed policy becomes eligible for activation only through a fixed cohort (design §15,
[ADR-0181](adr/0181-declare-cohorts-before-they-run-inject-only-the-candidate-and-count-whole-power-trials.md),
[ADR-0182](adr/0182-bind-each-calibration-member-to-its-first-dispatch-and-its-authoritys-plan.md)).
Every step below is a reviewed change or a dispatch, and every failure stays in the record.

1. **Declare.** On the proposed authority's merged SHA, declare the cohort and commit it under
   `fixtures/performance/` through a reviewed change before any member runs:

   ```sh
   python3 scripts/performance/cli.py cohort-declare --repo . --authority SHA \
     --cohort-id quick_2026_11 --tier quick --environment linux_x86_64_gnu_hosted \
     --recipe linux_gnu_native_release \
     --group hosted_x86 'CPU MODEL' 'ghcr.io/owner/image@sha256:DIGEST' \
     --start-date 2026-11-02 --days 5 --repository OWNER/NAME --branch main \
     --after-run-number N --output fixtures/performance/cohort-quick-2026-11.json
   ```

   `--after-run-number` is the `run_number` of the newest run of `performance.yml`, read just
   before declaring (`GET /repos/OWNER/NAME/actions/workflows/performance.yml/runs?per_page=1`),
   or 0 before its first run. Every later run of the workflow must still be listed when
   readiness is judged: do not delete any run of `performance.yml` while a cohort is open.
   Readiness refuses a history with a missing number, because the missing run may be a
   member's original (ADR-0183). The environment must be a GitHub-hosted one.

   `--group` takes three separate arguments: ID, exact CPU model and complete registry-pinned
   sample image. Quote CPU and image separately; `@` inside a CPU model or image is preserved.
   The former combined `ID=CPU@IMAGE` syntax is refused. Repeat the option for each group a runner
   may report. Each case uses the group observed by its paired shard; different shards may
   have different allowed CPU models. Readiness records that binding per case.
   `--repository` and `--branch` name the only producer of member runs: a
   `workflow_dispatch` of `.github/workflows/performance.yml` on that branch of that repository. The cohort fixes 20 A/A members on days 0-1, the preparation members on day 2,
   and 30 descriptive and 10 power trials on the remaining days. A changed policy or
   environment starts a new named cohort; the old one is kept.
2. **Run the A/A and preparation members**, each on its declared UTC day, by dispatching
   `performance.yml` with `scope: calibration`, `base`, `head` and `authority` all set to the
   cohort's SHA, `control` set to a trusted revision holding the cohort file, `cohort` set to its
   path, `cohort_digest` set to the digest `cohort-declare` printed and `member` set to the
   member ID. Also supply `dispatch_head`, the full main SHA whose workflow bytes were checked
   against the cohort authority. Dispatch to `ref: main`; the run must resolve to that SHA.
   Only then is it named `af-perf calibration DIGEST MEMBER` as it is created, before any
   job runs, so the first such run is that member's one original whatever becomes of it. Do
   not rerun one: a rerun or a second dispatch of the same member is refused as evidence, and
   the member keeps its original outcome, failed or missing. A dispatch whose digest is not the
   control's cohort, or that requests the dedicated executor, is refused by its plan job and
   stays that member's failed original. Only an original whose attempt 1 concluded `success`
   can count; any other conclusion leaves the member failed.
   An absent, malformed or stale `dispatch_head` instead names a non-member
   `af-perf source-mismatch` and refuses before checkout, planning, building or measuring.
   This guard binds the source; it does not make dispatch atomic. Future incompatible workflow
   edits still fail producer verification. For the disabled operator preflight, deployment and
   the required new cohort after this repair, see
   [the cohort dispatcher runbook](performance-cohort-dispatch.md).
3. **Seal the parameters.** Assemble one evidence directory, every file unedited:
   - `runs.json`: the workflow's runs of every event, newest first,
     `GET /repos/OWNER/NAME/actions/workflows/performance.yml/runs?per_page=100&page=P`,
     pages concatenated into one `workflow_runs` array beside the response's `total_count`,
     down to the first page that reaches the cohort's anchor run number. Every run number after
     the anchor must be present.
   - `attempts/RUN.json` for each member's first dispatch: its attempt 1,
     `GET /repos/OWNER/NAME/actions/runs/RUN/attempts/1`. Its own start, end and conclusion,
     never an artifact's upload time, place the member on its day, after the parameters, and
     decide whether it succeeded.
   - `jobs/RUN.json` for each first dispatch: attempt 1's jobs,
     `GET /repos/OWNER/NAME/actions/runs/RUN/attempts/1/jobs?per_page=100`. Each `build` and
     `shard` job must show the plan's hosted label, the `GitHub Actions` runner group and a
     hosted runner name.
   - `actions.json`: the artifact listings of every run named for the cohort,
     `GET /repos/OWNER/NAME/actions/runs/RUN/artifacts`, concatenated, and one subdirectory per
     downloaded `af-perf-<run>-run` and `af-perf-<run>-shard-<i>` artifact of the originals,
     named as the artifact. Two artifacts of one expected name invalidate that member.

   `cli.py activate --verify-proposed` assembles exactly this directory itself (below). Every
   listing must be complete: a page count over its bound, a `total_count` that changes between
   pages or disagrees with the items, or an item listed twice is refused, never shortened. Each
   original's head commit must carry exactly the authority's `performance.yml`, and its plan's
   control revision exactly the authority's `scripts/performance`; another producer leaves the
   member invalid (`producer_unverified`). A cohort declaration commit on top of the authority is
   expected: neither commit need equal the authority.

   Then, in a checkout holding the authority and every control the members were dispatched
   with, seal the parameters and commit them through a reviewed change before any trial. Each
   member's plan is frozen again from the authority and must be exactly the retained one:

   ```sh
   python3 scripts/performance/cli.py calibration-prepare --repo . \
     --cohort fixtures/performance/cohort-quick-2026-11.json --evidence EVIDENCE_DIR \
     --output fixtures/performance/calibration-quick-2026-11.json
   ```

   A case and group without a positive A0, or without a measured loop cost, has no loop count.
   That group is not ready, and its hosts refuse slowdown shards.
4. **Run the trials** as in step 2, adding `calibration` set to the sealed parameters' path.
   Both sides build with `perf-calibration`, and only the candidate runs the loop.
5. **Recompute readiness** over every member's artifacts:

   ```sh
   python3 scripts/performance/cli.py readiness --repo . \
     --cohort fixtures/performance/cohort-quick-2026-11.json \
     --calibration fixtures/performance/calibration-quick-2026-11.json \
     --evidence EVIDENCE_DIR --output readiness.json --job-summary summary.md
   ```

   It exits 0 only when ready, and 4 otherwise. It first recomputes the parameters from the
   A/A and preparation originals and refuses any sealed document that differs. Keep
   `readiness.json`, the summary and every artifact. The archive's
   `--pin --reason active_cohort` keeps them past transport expiry. A cohort that is not ready
   stays committed as it is: a changed policy, environment, threshold or workload starts a new
   named cohort, and the failed one is never edited or reused.

## Collection before expiry

Actions artifacts expire (90 days for trusted runs here). Activation needs the cohort's
originals' artifacts still listed and readable, because fetched verification recomputes
readiness from them, and an expired original makes its member missing and refuses
activation. Collect every member's artifacts into the archive as each member completes
(`archive --fetch ACTIONS_RUN_ID --lab-run gh<run>_a1_primary`, see
[the archive runbook](performance-archive.md)), pin the cohort's runs as `active_cohort`, and
open the activation change well before the earliest original expires. The archive is a
retention backup for investigation; it is never a source of activation.

## Activation

Activation is a reviewed change that commits `fixtures/performance/activation.toml`
(`af.perf-activation/3`, ADR-0183, ADR-0184) beside the cohort, the sealed parameters and the
readiness document. It names each record by path and SHA-256, every member's original run in ascending
order, and the environment, recipe and CPU/image groups the cohort calibrated: exactly the
groups whose parameters have A0 and a loop count for every critical case. Nothing writes it
automatically, and no `.af/` authority is installed by it.

1. **Verify the reviewed records** in the activation's checkout. Every active plan does this
   too, for every pull request, push and schedule, without fetching the cohort again:

   ```sh
   python3 scripts/performance/cli.py activate --verify --repo . --revision HEAD \
     --output verification.json
   ```

2. **Verify from the retained evidence.** The `Performance activation` workflow
   (`.github/workflows/performance-activation.yml`) does this on every pull request to `main`
   that touches `fixtures/performance/`. Its one job checks out the pull request's base
   revision as the trusted control and runs that revision's own `cli.py`, which reads the
   proposal only as Git objects; it builds and runs no candidate code, holds no secret, and its
   own token, scoped to `contents: read` and `actions: read`, reaches only this step:

   ```sh
   python3 -I "$RUNNER_TEMP/control/scripts/performance/cli.py" activate --verify-proposed \
     --repo . --base "$PR_BASE_SHA" --revision "$GITHUB_SHA" --repository OWNER/NAME \
     --token-env AF_PERF_ACTIVATION_TOKEN --output verification.json --job-summary summary.md
   ```

   When the manifest or any record it names differs from the base, it fetches the history,
   attempt and job records, complete artifact listings and the originals' bundles (and a
   bridge's two runs), recomputes readiness with its parameters and producers, and requires the
   reviewed document's members, classifications, doses, tallies, groups and decision exactly.
   Author-written success never activates. Missing, expired, capped or inconsistent evidence
   refuses. An unchanged activation is verified locally and its cohort is not fetched again. An
   operator may run the same command from a checkout of the base revision with a read-only
   token. Hosted execution of the workflow remains operational evidence.
3. **Review at the exact head.** The owner reviews the activation, its records and the
   verification at the exact head (the administrative prerequisite above). The verification
   lists that review as `pending`; only the owner discharges it.

A full plan whose environment or recipe the cohort did not calibrate is unavailable
(`uncalibrated_scope`); an allowed group the cohort did not prepare is reported as
uncalibrated, never as calibrated. An active plan carries the exact CPU models and images of
the calibrated groups (`af.perf-run/6`), and a measured shard observed on any other host, or an
unknown one, makes the verdict `incomplete` with `uncalibrated_cpu_group`: it cannot satisfy
the required check, while a regression there stays a failure. The cohort's denominators are
unchanged. Enforcement stays advisory until an operator makes a check
required, after the ruleset coexistence demonstration below.

### Equivalent adapters

A narrowly scoped API bridge keeps the calibrated cohort through an `equivalent_adapter`
admission. Its manifest keeps every record, run and scope of the calibrated activation, names
its new authority, and names the calibrated manifest retained in the control revision with
its SHA-256. Verification refuses a protected diff between the two authorities beyond the
named adapter files (which must lie in a closure of the named equivalence group),
`adapters.toml` entries of that group and the calibration records; the registry must be
byte-identical. The build closure counts too: every `Cargo.lock` and `Cargo.toml`, every
`build.rs`, the toolchain pin and any `.cargo` configuration, wherever added, must be unchanged.

A bridge measures as active only with verified execution evidence
(`af.perf-equivalence/1`, committed beside the manifest and named by path and SHA-256):

1. **Golden check on both product sources.** Dispatch `performance.yml` with `scope`
   `bootstrap`, `base` the calibrated authority, `head` the bridged authority and `authority`
   the bridged authority. Both builds must compile adapters of the named group, the bridged
   product through the bridge's adapter, and every planned sample of every cohort case must be
   present and accepted by its golden oracle.
2. **Post-merge A/A.** After the bridge merges, dispatch the same with `base` and `head` one
   source descending from it. It must run through the bridge's adapter on a calibrated CPU/image
   group, healthy and complete, every critical case within tolerance.
3. **Compute the record** from the two retained run directories:

   ```sh
   python3 scripts/performance/cli.py equivalence-evidence --repo . \
     --calibrated-activation fixtures/performance/activations/first.toml \
     --authority BRIDGE_SHA --group native_probe_public_api_v1 \
     --adapter-file crates/review-pipeline/examples/perf-probe/cases.rs \
     --golden GOLDEN_RUN_DIR=ACTIONS_RUN --post-merge-aa AA_RUN_DIR=ACTIONS_RUN \
     --output fixtures/performance/equivalence-bridge.json
   ```

   It exits 0 only when both checks verify. The activation workflow recomputes the record from
   the two runs' retained artifacts and refuses any difference. A matching file list is not
   equivalence: the owner's semantic approval stays `pending` in every verification.

## Cost and shipping demonstrations (pending)

Design §15 needs five real pull-request comparisons and five same-source comparisons, each
with a cold and a warm build cache, and twenty shipping samples per discovery command on the
named Linux and macOS hosts and per state read on Linux, each strictly below 50 ms or 200 ms.
Keep every run's directory, failed ones included (or fetch it from the archive), then:

```sh
python3 scripts/performance/cli.py cost-report --run RUN_DIR --run RUN_DIR ... \
  --output cost.json --job-summary summary.md
python3 scripts/performance/cli.py shipping-report --suite fixtures/performance/suite.toml \
  --run NIGHTLY_LINUX_RUN --run MACOS_RUN ... --output shipping.json
```

Build each cost run with `cli.py build --build-cache cold` (the default) or `--build-cache
warm`. Cold requires every worktree target to be observed empty before the measured pass; warm
first primes the run's own worktree targets with a bounded build of the same sources, recorded
apart, and requires them primed (`af.perf-builds/6`). A prefetched Cargo home is dependency
preparation, reported as `dependency_fetch`, never a warm build cache. The cost report reads each
run's compiled cache from its build manifest; `--run DIR=cold` or `DIR=warm` must agree, and only
names the cache of a run whose manifest was not retained. Every execution failure makes a failed
attempt, kept with its costs and failures and never counted: a failed build or shard, a missing
shard or record, a failed or timed-out sample or one without its required metric, a failed
health sentinel or unstable shard, and any integrity refusal. `cost-report` exits 0 only when
each of the four groups has five completed measured executions. Each retained run and bundle
counts once, so five same-source runs at one SHA are five; a healthy completed comparison that
found a regression counts. `shipping-report` refuses a run planned under any policy but the
given suite's, and a run or run bundle given twice. It never pools product identities: each
budget instance reports one target per candidate SHA, shipping binary, recipe and observed
hardware/image fingerprint, with its own sample count and exact maximum, median and nearest-rank
p95. It accounts for every planned measured candidate sample. A run whose build, integrity or
shard health is not valid and healthy leaves its instances incomplete, its samples shown. It
exits 0 only when every target of every budget instance has its minimum measured samples, none
failed or missing, every one strictly below its limit (exit 3 when one is not). A missed shipping
target is optimization work, never a relaxed limit. Until both are produced from real runs they
remain pending (`af.perf-cost/3`, `af.perf-shipping/3`, ADR-0185).

## Guarded kernel experiments

`fixtures/performance/kernel-measure/` stages the guarded measures of design §13 for a reviewed
human installation into `.af/code-policy.toml` (its README, ADR-0185). Each runs the literal
isolated bootstrap of `scripts/performance/perflab/kernelguard.py` with the protected-harness
digest it expects; the bootstrap refuses a changed, missing or extra protected file, a link or a
special file before the driver is imported or anything is built, then `cli.py measure` supplies
one golden-judged library sample per kernel repetition. Protected names match caselessly, a warm
target is keyed by the whole measured Snapshot, and the probe's streams and the build log are
bounded while they are written (ADR-0186). `make check` runs `cli.py harness-digest
--check`, which fails while the staged files are stale or an installed guarded measure differs;
re-pin with `cli.py harness-digest --write` after any change to the driver, fixtures, probe,
adapters, trace or calibration helper, Cargo manifests, the lock, build scripts, the toolchain
pin or Cargo configuration, including every dependency update and version bump.
`scripts/release.sh` re-pins the staged files in the release commit. The staged objectives
compare `elapsed_ms`; the inner `wall_ms` stays pending until the guard's kernel integration
(`crates/af/tests/it/perf_kernel_measure.rs`) is accepted on a sealed source and a reviewed
change selects it.

## Ruleset and release-pin coexistence (pending)

Before `Performance check` becomes a required check, export the `main` rulesets
(`gh api repos/OWNER/NAME/rulesets` and each ruleset by ID) before and after the proposed
change, and in the staging repository demonstrate that the release workflow's authorized
release-pin push still lands with the check required. Keep both exports and the release run as
evidence. If the push is blocked, stay advisory and propose a separate release and ruleset
decision. Nothing in the laboratory changes a ruleset or branch protection, and no AF Task
receives a publishing credential for this.
