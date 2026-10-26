# Performance in the development lifecycle

The repository starts in advisory mode. `Performance check` is the stable final result; its
summary explicitly says whether the comparison qualifies. Green advisory execution alone does
not establish that a change is fast. [The laboratory design](design/continuous-performance.md) covers local
comparisons, traces and flamegraphs; [the CI runbook](performance-ci.md) covers calibration.

## Pull requests and main

Every PR to main compares its exact merge against the base, using the base's trusted harness.
Every main push compares against its first parent. Before activation, these are bootstrap
measurements. Review the performance summary and download `perf-report-*` for the interactive
report, per-case changes and evidence. Missing builds, unstable hosts and inconclusive samples
remain visible. A PR that changes the harness also needs review of that change.

Nightly runs compare main with the highest reachable stable release. Published releases are
measured through a separate read-only release handoff. This happens after publication; use the
evidence to investigate changes and plan the next release, not as proof of a publication gate.

For optimization experiments, dispatch `Performance` with explicit full base/head/control SHAs,
`scope=partial_scope`, a tier, cases and pair count. Keep the baseline fixed while comparing
alternatives. Use a separate diagnostic child run for CPU profiles and phase traces; its
instrumentation overhead must not enter the timing verdict. Promote a successful experiment
through a normal PR and the full comparison.

Telemetry's own cost is measured beside the comparison by the advisory `Performance telemetry`
workflow on pull requests, nightly, on publication and on dispatch, and a diagnostic child can
record its CLI cases' telemetry with `--telemetry spans`; neither touches the verdict
([`performance-telemetry.md`](performance-telemetry.md)). That workflow measures only in
containers of the image `AF_PERF_GNU_BUILD_IMAGE` pins and plans nothing without it.

## The no-perf label

A pull request to `main` labeled exactly `no-perf` is not measured: no plan, build or shard runs,
and `Performance check` reports `exempt (label no-perf), not measured`, which is an exemption and
never a measured or qualified result. Adding the label cancels a running measurement; removing
it measures again. Only the exact lowercase name counts, only on the run's first attempt (a
rerun measures), and pushes to `main`, the nightly schedule, dispatches, calibration and release
handoffs ignore it
([ADR-0196](adr/0196-skip-pull-request-measurements-only-for-an-explicit-no-perf-label.md)).
Ordinary correctness checks, including the laboratory's own unit and schema tests in
`validate.yml` and `make check`, run whatever the label says.

Agents opening or updating an ordinary Afactory pull request apply it conservatively:

1. Inspect the entire diff against the base, not the latest commit or the task's intent.
2. Label only when every changed file is prose — an ADR, a runbook, a design note, a change note
   — or a similarly clear non-executable edit that no build, test, benchmark, af runtime input
   or embedded file reads. Markdown is not prose by its extension: a crate's `README.md` can be
   compiled in, and `.af/` prompts are runtime inputs.
3. Keep the measurement for code of any size, however small or easy; dependencies, lock files
   and build settings; the performance harness, fixtures, golden data and benchmarks; CI
   workflows and performance policy; `.af/`; and every mixed or uncertain change.
4. Say in the pull request description why the change has no runtime impact.
5. Re-evaluate on every update, and remove a `no-perf` the new diff no longer justifies.

A change to the laboratory, its workflows or its policy is never labeled: it is exactly what the
measurement has to see. Reviewers check the label against the diff like any other claim. Remote
Check gate pull requests target `af-gate/**` bases and are never measured here; the label af
puts on them for its operators' `no_perf_paths` is described in
[Remote Checks](task-execution/remote-checks.md#3-give-this-machine-a-push-target).

## Infrastructure rollout

1. Merge the laboratory with ordinary validation. The installation PR and first push explicitly
   report that their trusted base predates the laboratory.
2. Dispatch `Performance images` from main. Both images must pass tool-version and anonymous
   pull checks. GHCR packages must be public for credential-free measurement; if the registry
   creates a private package, change its visibility and repeat the provisioning check.
3. Set `AF_PERF_GNU_BUILD_IMAGE` and `AF_PERF_MUSL_BUILD_IMAGE` to the verified `NAME@sha256:...`
   references from its artifacts. Never set them to a tag or machine-local image ID.
4. Dispatch a same-source partial comparison. Confirm the verified 2-GiB Linux scratch, complete
   shard evidence, report, collection and compatible history group. Then dispatch a complete
   quick bootstrap comparison with no case filter and the registry's pair counts. Confirm every
   planned record, including all closing sentinels, before starting calibration. A partial pilot
   proves the transport path but cannot establish that the full suite fits its schedule.
5. Declare the five-day cohort, commit its members before dispatch, and follow the preparation,
   sealed-parameter, injection and power sequence in the CI runbook. Failed originals count;
   they cannot be replaced with convenient reruns.
6. Review activation only when retained originals establish readiness. Export branch rules,
   arrange owner review at the exact head, and merge the enforcement policy change. Then require
   `Performance check`. Until then keep `fixtures/performance/enforcement.toml` advisory. A
   required check still passes a `no-perf` exemption; decide then whether everyone who may label
   a pull request should keep that power.

## Durable collection

Use a persistent location outside any disposable worktree:

```sh
python3 scripts/performance/cli.py archive --root /srv/af-perf/archive --init
python3 -I scripts/performance/collect.py --root /srv/af-perf/archive \
  --history /srv/af-perf/history --repository OWNER/NAME --gh-auth
```

`--gh-auth` reads the host's existing GitHub credential without printing it. A dedicated
read-only token can instead be supplied through `AF_PERF_ARCHIVE_TOKEN`. No product process
runs in the collector. Schedule the command hourly with a 35-minute outer timeout; each scan
has its own 30-minute network deadline. It scans 13 days by default, refuses capped or changing
listings, retries failed imports, pins calibration runs, applies retention, and updates
`history/history.html` and `history/archive-status.json`. For a longer catch-up, explicitly
increase `--lookback-days` up to 89; expired transport cannot be recovered by retrying.

Telemetry experiment results have their own lane, never this archive: `cli.py
telemetry-collect --config FILE` keeps each `perf-telemetry-*` result verified under its exact
measurement authority in a separate, locked and bounded history root. Its example configuration
and systemd units are in `scripts/performance/service/`, for a future always-on Linux host; none
is installed ([`performance-telemetry.md`](performance-telemetry.md)). This collector behaves as
before (its bounded ZIP reading is now shared with that lane), and the deployed one stays pinned
to the frozen cohort's control.

A local Mac service collects while awake. Its exit status and logs, plus archive health, must
be monitored; after a long outage inspect the scan before transport expiry. For unattended
operation, move the same service and archive to an always-on host with persistent storage.

## Scope of the retained demonstration

The committed local measurements describe the original candidate's recorded source fingerprint.
They do not measure the integration changes, Linux hosted performance, or a calibrated policy.
The original final AF Task was incomplete following cache cleanup failure; its semantic
reviewers did not run. The integration uses ordinary validation and records those limitations
instead of claiming native AF acceptance.

## Python validation

`validate.yml` runs the laboratory tests and schema parity once on the latest stable Python,
selected with `actions/setup-python` using `python-version: '3.x'` and `check-latest: true`.
Tomli/tomllib parity is mandatory. The required `lint` check fails when this job fails, is
cancelled or is skipped. The other validation jobs use the same Python selection before
executing Python or Make commands ([ADR-0197](adr/0197-validate-on-one-latest-stable-python.md)).

## GA source integration checks

The integrated producer retains GA's 60,000 ms production lease, exact current suite/goldens,
Task continuation and gate retry/reporting alongside the lab's diagnostic and containment
contracts ([ADR-0221](adr/0221-integrate-the-performance-lab-with-ga-and-control-short-attempt-fixtures.md)).
The two reported Store dispatch failures use a separate 1,000 ms Attempt allowance. Their
fixtures now control Store time locally; no production deadline or allowance is extended.
Reproduce the repair and its neighboring expiry/accounting cases from the repository root:

```sh
cargo test --locked --workspace --lib store::task::
PYTHONDONTWRITEBYTECODE=1 python3 -B scripts/nextest-gate.py --repeat 20 \
  --locked --workspace --lib --profile ci --test-threads 4 \
  -E 'test(rejected_task_output_keeps_its_charge) or test(captured_retry_policy_checks_durable_failures) or test(short_attempt_fixture_paths) or test(real_store_dispatch_deadline_boundary) or test(renewal_race)'
PYTHONDONTWRITEBYTECODE=1 python3 -B scripts/performance/schemas.py
PYTHONDONTWRITEBYTECODE=1 python3 -B scripts/performance/cli.py harness-digest --check
```

Use synthetic HOME/XDG directories and the pinned Rust 1.88 toolchain for AF checks. Repeat
mode stops on any failure and never substitutes an isolated retry for a successful repetition.
The ordinary native gate retains its separate failed-test isolation policy and reports flakes.
Regenerate protected inventory/staged policy with `harness-digest --write` after protected
edits; `.af/` authority is unchanged by this integration. Full native verification and independent
reviews run on sealed source. Follow the [dispatcher runbook](performance-cohort-dispatch.md)
only after acceptance and exact commit; the retained old cohort is unchanged, and this source
integration declares no replacement cohort or deployment.
