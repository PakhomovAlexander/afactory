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

## The no-perf label

PR validation and measurement share the selection in [the test-class design](test-classes.md).
The PR default is functional + e2e + uix; perf is opt-in. `no-perf` excludes both performance
validation and the separate measurement workflow. `test:perf` opts into perf as an explicit
allowlist; `release:full` or all four positive labels select the complete union. Conflicting,
unknown reserved or empty selections fail. A perf exclusion is reported as **EXEMPT (perf class
NOT RUN; not measured)**, never a measured pass. Label changes cancel older PR measurements.
Reruns preserve the event selection. Push, schedule, dispatch and published-release handoffs
retain their prior measurement policy and ignore PR exclusions.

Inspect the entire diff when choosing coverage, explain the decision in the PR description,
and re-evaluate it on updates. ADR-0213 explicitly supersedes the older prose-only recommendation
for ordinary repository PR labels. The kernel's Remote Check `no_perf_paths` mapping remains
prose-only and unchanged; those gate PRs are never measured here. See
[Remote Checks](task-execution/remote-checks.md#3-give-this-machine-a-push-target).
Local `make check` remains full. Selected perf validation requires the lab's latest-stable-Python
and schema parity checks; excluded perf is visible as NOT RUN. Release publication requires its
bound validation union and packaged-binary smoke. Post-publication measurement remains asynchronous
and advisory, including after full releases.

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

`validate.yml` runs selected perf laboratory tests and schema parity once per selected platform on
latest stable Python, using `python-version: '3.x'`, `check-latest: true`, and mandatory Tomli/tomllib
parity. Stable lint/check aggregators require all selected class receipts, failing for missing,
cancelled or skipped selected work. Every validation job uses the same Python selection before
Python or Make commands. ADR-0213 replaces ADR-0197's unconditional lab dependency with this explicit
selection; full validation still requires the complete lab.
