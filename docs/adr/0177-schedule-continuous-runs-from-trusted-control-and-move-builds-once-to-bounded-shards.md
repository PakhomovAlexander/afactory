# ADR-0177: Schedule continuous runs from the trusted control, and move builds once to bounded shards

**Status:** accepted (2026-10-20). Implements package p6a (PR, main, nightly and dispatch
scheduling with trusted measurement authority) of the
[continuous performance laboratory](../design/continuous-performance.md). It first repairs the
gate failures and root findings left by package p5
([ADR-0176](0176-diagnose-cli-cases-and-contained-linux-runs-and-bound-every-step-and-seed-before-it-is-used.md)),
whose decisions stand except where this ADR amends them.

| Contract | Version | What changed |
| --- | --- | --- |
| Schedule | `af.perf-schedule/1` | new |
| Build pack | `af.perf-build-pack/1` | new |
| Report | `af.perf-report/2` | replaces `/1`, which is refused |

## Context

The p5 completion's sealed source failed the mandatory gate with three Python failures, and root
checks confirmed three findings on it:

1. **A repetition count from the scheduler.** `Region.test_short_regions_repeat_as_fresh_executions…`
   inferred four repetitions from a 0.2-s capture of 0.05-s regions; control latency left two.
2. **Clocks that disagreed.** Two `HostCancellation` tests stood the monotonic clock still at 0
   and 57 s but derived the caller's deadline from the real wall clock. The plan and build
   reads took real time before the deadline was read, so a loaded host had less than the 3 s
   the tests assumed, and the run refused its launch as `run_deadline`.
3. **An uncertain release removed what it kept** (`p5-root-uncertain-cleanup`).
   `Profile.release` removed the namespace before it checked uncertainty, then named the absent
   path as kept.
4. **A trace outside the evidence room** (`p5-root-cli-trace-budget`). A traced execution wrote
   its raw trace into its namespace, which the evidence counter excludes. With 2,048 bytes of
   room, an 8,192-byte trace was admitted, and its overage counted as zero after cleanup.
5. **A rerun that planned another experiment** (`root-report-reproduction-1`). The report's
   rerun reset the authority to the control's, the shards to the tier's, and the pairs to the
   registry's. It truncated a selection to 20 cases and omitted the build mode. The report
   always marked profiles and traces unavailable, and a global diagnostic omission's command
   named neither `--case` nor `--verdict`.

Package p6a itself needs the event resolution, authority and transport that make the laboratory
run continuously without trusting candidate code.

## Considered options

1. **Lower the repetition assertion or lengthen the capture.** Rejected: that hides the race.
2. **Run the scheduler from the candidate checkout.** Rejected: a candidate would choose its own
   baseline and authority. The control revision's worktree runs every script.
3. **Fall back to a trusted host build when no image is configured.** Rejected: CI builds only
   in a registry image pinned by digest, and a missing one is `no_build_image`.
4. **Upload the build directory as an artifact.** Rejected: Actions' ZIP drops every mode and
   trusts member names. A tar keeps them, and only verified bytes regain their modes.
5. **Rerun one failed shard job.** Rejected: a shard runs once, and a rerun is a new run.

## Decision

1. **Gate repairs.**
   - **The repetition count is the driver's.** The test's `on_next` asks for exactly four
     fresh executions under a 20-s cap. It asserts each region's recorded interval, its setup
     before its enable, the messages, the acknowledgements and the reset state. A separate test
     keeps the real cap, `stopped == "capped"`.
   - **Coherent clocks.** The `HostCancellation` wall clock stands still at the instant the
     caller's deadline was fixed, so every launch has exactly 3 s. A 1.5-s real preflight is
     now part of both uncertain-cleanup tests. They assert a 2.200-s window, the launched
     supervisor, the live child and the kept scratch. The no-launch refusal test is unchanged.
2. **Uncertainty decides before removal** (`Profile.release`). An uncertain end keeps the
   namespace and its data, closes its handles, and names the path. `halted` then refuses every
   later namespace and tool launch. `run_profile` exits 4 and prints the kept paths. Only a
   confirmed end removes the namespace. The run's final cleanup already keeps its whole work
   directory once anything is kept.
3. **A raw trace is evidence while it is written.** CLI and native traces are staged below the
   evidence's `.trace-stage/` (the stage's when contained). The watch counts them during the
   step, and the step is settled before admission. A trace over the room is refused as
   `trace_over_cap` with the bytes it held. It stays counted (moved into the evidence from a
   contained stage) and appears in `evidence_overage`. An admitted trace is unstaged and charged
   again only when kept. Seed and setup scratch stay outside the count, and the native probe
   keeps its file limit at the room.
4. **Faithful reproduction** (`reproduce.py`).
   - **Report reruns.** A rerun keeps the trigger and its event fields. It keeps the resolved
     baseline, candidate and control, and the authority exactly as selected: an explicit SHA
     for proposed and active full plans, none for the control's or a re-read activation. It
     also keeps every partial instance, shards, uniform pairs, the seed, the environment, the
     build mode, and a calibration's member with `COHORT_JSON`. Each rerun lists its
     limitations.
   - **Diagnostic omissions.** A diagnostic omission's command keeps the selection: the named
     cases, or `--verdict` with `--max-cases`. It also keeps the frozen duration, frequency and
     wall bound, the recipe, mode, build inputs and tools. Host paths are documented
     placeholders.
5. **Linked diagnostics** (`af.perf-report/2`, `report --diagnostic DIR`).
   - **Admission.** A child bundle must verify. Its `profiles.json` and frozen child plan must
     name this run, plan and sources, and every artifact it names must be a verified member
     with that hash. Anything else is refused before the report writes a file.
   - **Display.** A verified copy goes below `diagnostics/<child>/`. Each case's profile and
     trace state is `unavailable`, `available`, `partial` or `omitted`. It links the
     flamegraphs, trace, Perfetto export and timeline, and states coverage, overhead labels and
     omissions. The summary shows the same states. The parent's bundle never changes.
6. **Scheduling** (`ci.py`, `ci-schedule`, `af.perf-schedule/1`).
   - **Resolution.** Each event resolves once to full SHAs. A PR's merge is compared with its
     first parent, which must equal the event's base. A main push is compared with its first
     parent, and a nightly with the highest reachable stable tag. A dispatch takes full SHAs
     or fully qualified refs only.
   - **Skips.** `af-gate/**` and other bases are skipped. So is any other event, a
     `pull_request_target` included.
   - **Authority.** The primary comparison is full scope under the control's activation. Without
     one it is `no_active_authority`, and an advisory control bootstrap runs instead. A policy
     change adds the two proposed-policy bootstraps, (base, merge) and (merge, merge), and a
     dispatched calibration plans from the control's cohort file without an activation.
   - **Trust.** A PR is untrusted and runs only on ephemeral hosted runners. Dedicated capacity
     needs a trusted event and a configured label. Enforcement is the constant `advisory`.
7. **Build once, verified shards.**
   - **Packing.** `ci-pack` writes a deterministic `build.tar` and `af.perf-build-pack/1` from
     the admitted build directory.
   - **Unpacking.** `ci-unpack` first checks the plan, the archive's hash, member types, paths,
     duplicates, listing and sizes. It writes every file new, hashed while written, and
     restores 0555 or 0444 only after all bytes verify.
   - **Transport.** Shards stage their evidence, failed or not, in the unchanged
     `af.perf-transport/1` layout as `af-perf-<run>-shard-<i>`.
   - **Aggregation.** `ci-aggregate` refuses foreign, unplanned or duplicate shards and keeps
     missing ones as incomplete. It writes the summary from the validated report, stages
     `af-perf-<run>-run`, and is advisory unless `--enforcement required`.
   - **Collection.** One Actions run may hold several laboratory runs, so
     `archive --fetch --lab-run RUN` collects one run's `af-perf-<run>-*` artifacts.
8. **Credentials and hosted scratch.**
   - **Credentials.** `ci-credentials` refuses credential sections, headers, includes and
     user-info URLs in `.git/config`. It also refuses credential variables, token-shaped values
     and token-shaped bytes in packs and bundles, and it never echoes a value.
   - **Scratch.** `ci-scratch` loop-mounts a 2-GiB ext4 image with passwordless sudo before any
     candidate code runs. It refuses without Linux, without that sudo, or below the 10-GiB floor
     plus the image.
9. **The workflow** (`.github/workflows/performance.yml`). It has `plan`, `build`, `shard` and
   `aggregate` jobs. The workflow has read-only permissions and no secrets, and every action is
   pinned to the existing reviewed commits. Every checkout has `persist-credentials: false`,
   and every script runs from the control's worktree. Jobs have timeouts, and shard concurrency
   is the plan's. Retention is 14 days for PRs and 90 otherwise. Images come from operator
   variables. The Python 3.9 and 3.11+ parity job in `validate.yml` is unchanged and still
   required through `lint`.

## Consequences

- **Tests.** `test_ci` drives fixture events over a Git history: fork without trust, wrong event
  base, unsupported parent, no activation, harness and policy changes, gate and other bases,
  missing images, dispatch refs and calibration. `test_continuous` packs a stand-in build,
  moves it through a ZIP, unpacks it, runs two shards from it, and aggregates with missing,
  failed, foreign, unplanned and duplicate artifacts. It also covers hostile tars, credentials
  and the scratch steps. `test_performance_workflow` checks the workflow's text and runs its
  control-resolution and rerun-guard shell. `test_reproduce` runs generated rerun commands
  through the planner and compares the plans. `test_report_diagnostics` links real child bundles
  made by the stand-in driver and refuses tampered, unsafe and unrelated ones.
  `TraceRoom` and `UncertainCleanup` cover the two root findings both ways.
- **Pending operational evidence.**
  - **Hosted execution.** No hosted run of the workflow has happened. A real run must still
    show that the image builds on the runner, that the scratch filesystem verifies, the
    capacity and cost, and collection before expiry.
  - **Registry images.** The verified registry images are an operator prerequisite
    (`docs/performance-ci.md`). Until they are set, comparisons are `no_build_image`.
  - **Release handoff.** Release handoff and activation verification are package p6b.
- **Not changed.** The 15-second lease, the 8-MiB envelope cap, every verdict bound, fixed
  per-case N and sentinel schedules, the 10-GiB floor, rulesets and branch protection.
