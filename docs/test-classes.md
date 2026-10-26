# Validation test classes

This design governs repository development and release validation. It changes no kernel runtime,
Task authority, individual assertion, deadline, or nextest exclusive scheduling rule.
[ADR-0213](adr/0213-select-validation-classes-and-bind-release-profiles-to-tags.md) supersedes the
always-full CI policy in ADR-0107, the unconditional latest-Python lab dependency in ADR-0197,
and the repository operator's prose-only recommendation for `no-perf` in ADR-0196. The AF kernel's
remote gate label authority and `.af/` are unchanged. The required AF/local `make check` remains full.

## Classes and catalog

Every test has exactly one primary class:

| Class | Meaning | Examples |
| --- | --- | --- |
| functional | Component and public contract correctness | Typed schemas, config admission, graph compilation |
| e2e | Complete workflows, lifecycle, subprocesses and containment | Task delivery, Campaign rounds, process cancellation, live containers |
| uix | CLI/TUI interaction, output, help, errors and navigation | `cli_surface`, `help_hygiene`, `task_file_errors`, `tui` |
| perf | Measurement, the performance laboratory and expensive performance integration checks | `perf_kernel_measure`, `perf_probe`, optimizer and telemetry overhead tests |

`.config/test-classes.json` is the versioned, reviewable ownership catalog. Consolidated integration
binaries list every top-level module explicitly, including fixture-only modules. Adding or removing
a module requires a catalog update even when that module currently has no tests. Standalone binaries
are registered explicitly. A newly introduced binary fails the compiled inventory audit. Registered
component/unit binaries explicitly assign new internal modules to their default class; overrides
name UIX, performance or lifecycle submodules. This is a documented component rule, not a duration
heuristic. Whole workflow modules remain e2e even when they assert rendered output: for example,
`project` creates and removes worktrees and `task_report_command` runs real Tasks. `render` verifies
the Worker input contract. Neither is UIX merely because it invokes a CLI.

Every top-level Python test script is registered, with utility entrypoints separately enumerated.
The performance laboratory's unittest discovery assigns its entire subordinate suite to perf;
its existing discovery contract admits new lab test modules. Base policy tests are functional but
always run, independently of class selection. Their script names are recorded in `always_on_scripts`.

`cargo nextest list --workspace --locked --message-format json` supplies actual binaries, tests and
ignored state. The classifier refuses unknown identities, overlapping overrides and empty inventory.
It partitions every identity once, including ignored tests, then generates bounded nextest filters
from the audited inventory. Tests in each root module are grouped only when their selection agrees;
other names stay exact. Explicit singleton overrides classify manual measurement tests as perf.
Every ignored identity also records its manual/child/container policy; a new ignored test fails
the audit until reviewed. Feature-on trace/calibration checks are additional perf build variants of the
original lint checks, with distinct retained JUnit files. Default-off Rust builds remain unchanged.

Exclusivity is orthogonal: `.config/nextest.toml` remains authoritative. Its real-time tests retain
all threads and their original deadlines within their class job. Classes use separate runner jobs.
The full union includes the former preflight Python checks, performance schemas and harness digest,
ordinary Rust suite and feature-on perf checks. The lab runs once per selected platform on latest
stable Python, with `AF_PERF_REQUIRE_TOMLLIB=1`; it is no longer repeated before Rust validation.

Ignored tests retain the previous policy. Benchmarks, child-process helpers and namespace-only probes
are inventoried as ignored, never silently promoted or discarded. Live container probes are an explicit
e2e job on Linux; missing Docker fails it. `make review-kernel-container-probes` runs exactly the three
existing probe selections, including their ignored tests. They remain outside default local
`make check`, as before; selective e2e execution includes them and requires a live daemon.

## Selection and precedence

Labels are case-sensitive data, passed through JSON or quoted arguments, never evaluated as shell.
`test:*`, `no-*`, and `release:*` are reserved namespaces: unknown names in them fail validation.
Other labels have no selection effect. Both label addition and removal rerun validation and replace
older PR runs through their existing concurrency group.

1. Reject any `test:CLASS` with `no-CLASS`, and reject `release:fast` with `release:full`.
2. Defaults are functional + e2e + uix for a PR; all four for local, ordinary main, AF gate PRs targeting `af-gate/**`, release PRs named
   `release/vX.Y.Z`, and an unconfigured release. The event base ref selects the gate context, preserving the Remote Check full default. `release:fast` changes the default to functional + e2e + uix.
3. Any positive labels form an explicit allowlist replacing those defaults:
   `test:functional`, `test:e2e`, `test:uix`, `test:perf`. With no positive label, use the default.
4. Subtract `no-functional`, `no-e2e`, `no-uix`, `no-perf`. An empty result fails.
5. `release:full` forces all four and refuses any negative or partial positive allowlist. Explicit
   positives naming all four are allowed. `release:fast` permits a custom positive allowlist or
   further exclusions; the retained profile is then named custom unless its union equals fast/full.

| Labels | PR selection | Profile |
| --- | --- | --- |
| none | functional, e2e, uix | fast |
| `test:functional`, `test:e2e` | functional, e2e | custom |
| `test:perf` | perf only | custom |
| `no-uix` | functional, e2e | custom |
| `no-perf` | functional, e2e, uix | fast |
| `release:full` | all four | full |
| `release:full`, `no-perf` | refused | none |
| `test:perf`, `no-perf` | refused | none |

Fmt, ordinary and diagnostic-feature clippy, source/catalog/selection validation, release-resolution
and pin policy checks, and the binary version smoke are always-on base work. Their costs are retained
separately; a perf-only plan still performs this work. Release archive, signature, architecture and
actual packaged-binary onboarding/planning smoke always run for every shipped target.

`validation / lint` and `validation / check (ubuntu-latest)` retain their exact branch-protection and
Remote Check names. They are honest aggregation jobs with `always()`. They require resolver and base
success, successful selected class matrix, exact SHA/catalog/plan receipts for every selected platform
and class, and live containers when e2e is selected. Missing outputs, skipped selected work, failure
and cancellation fail. Class exclusion appears as **NOT RUN**, with its reason, never as a passed test.
An empty or invalid plan cannot form a self-approving green gate. Releases also aggregate macOS.

## Release binding

`make release VERSION=X.Y.Z COMPAT='...' PROFILE=fast` passes `--profile fast` to `release.sh`;
omitting PROFILE uses full. The script applies `release:fast` or `release:full` to the release PR.
Before any branch, source change, commit or push, it checks that the requested profile label exists
and is readable. Provision the ten definitions explicitly (this Task never executes remote writes):

```sh
make test-labels          # dry-run: print the ten gh label create commands
make test-labels APPLY=1  # operator action: create/update those definitions idempotently
# Or: python3 scripts/test-labels.py --apply --repo OWNER/REPO
```

The command sets only these known names, descriptions and colors; it neither assigns PR labels nor
alters unrelated definitions. Names derive from the resolver's canonical class list; offline parity
checks keep all ten documented: `test:functional`, `test:e2e`, `test:uix`, `test:perf`, `no-functional`,
`no-e2e`, `no-uix`, `no-perf`, `release:fast`, `release:full`.

Change labels directly on the release PR to rerun selected CI automatically. Remove `release:full`
before choosing exclusions or a partial allowlist. No policy file, metadata commit or additional
approval is required. CI checks the release branch/workspace version and resolves the event's labels.

On the exact version-bump commit, **only when its version tag is absent**, `release-test-policy.py`
reads GitHub's commit-associated PR API with `gh api` and explicit pagination. The resolver job has
`pull-requests: read` and its existing tag-creation `contents: write`; `GH_TOKEN` comes from the runner.
Only one merged PR may supply labels: its `merge_commit_sha` must equal the exact triggering SHA,
its base must be `main`, and its head must be `release/vVERSION`. This same test covers GitHub's
merge, squash and fast-forward results; no ancestry guess or latest-unrelated-PR lookup grants
selection authority. Unmerged PRs, wrong versions/branches/bases and other SHAs supply no labels.
No match defaults full. Multiple exact matches, API errors, malformed responses, pagination overflow
and invalid label selections fail before creating/pushing a tag.

Before pushing, the resolver freezes `af.release-tests/1` JSON in the annotated tag: exact SHA,
version, source PR repository/number/merge SHA/base/head/merge time (or null), captured labels and
complete resolved plan including catalog digest. The pushed remote tag object must equal the local
object. Competing creators never force or replace a ref. Subsequent main retries and tag-triggered
runs read this annotation and never query moving labels. Malformed/stale bindings, changed catalog,
wrong commit, moved local tag or tag/version mismatch fail closed. The same plan drives Linux/macOS.
Labels are captured at first tag creation, not historically at the merge instant: a label change
between merge and first creation affects that initial capture. Once the annotation exists, later
label changes and even API outages cannot change its selection or block retries.

A direct lightweight tag or manually annotated tag without this binding defaults full, regardless of
associated PR labels or stale metadata files. Ordinary main commits also run full. A tag creation race
that leaves this version on another commit remains ordinary main validation, as before. There is no
new manual publishing path; an operator pushing a tag uses the existing tag workflow. Rerunning that
workflow uses the same immutable annotation. Validation artifacts are scoped to the current run
attempt. Rerun all jobs: a failed-jobs-only retry lacking current-attempt resolver/base/class
evidence fails rather than consuming stale artifacts. Disposable Git remotes and offline API fixtures
exercise these paths; tests strip runner identity/credentials and always install a fake `gh`.

Release notes and the retained `release-test-selection` artifact name the profile, selected and
excluded classes, exact version/SHA and workflow evidence URL. Fast/custom releases explicitly do not
claim full tests. Publication still joins all selected OS gates and all build/smoke/signature jobs.

## Separate Performance measurement

For PRs, the Performance workflow uses the same resolver from the event's **trusted base** checkout;
candidate code never runs in its scope decision. Default fast PRs, partial allowlists excluding perf,
and `no-perf` are explicitly **EXEMPT (perf class NOT RUN; not measured)**. `test:perf` includes
measurement; use all four positives or `release:full` to combine it with all validation classes.
Contradictions fail scope, not exempt it. A rerun preserves the event's selection. Label changes
start a new run and cancel the old measurement. During rollout, a base without the resolver retains
the conservative prior measurement behavior and exact `no-perf` exemption.

Only validation of the perf subsystem is required by these stable gates. Paired Performance
measurement retains its existing advisory/trusted-control enforcement and is **not** a release
publication dependency. Push, schedule, dispatch and published-release handoff measurements ignore PR
exclusions and retain their prior policies. Full releases require all validation tests, including
perf, but do not claim to have awaited asynchronous post-publication measurement. This distinction
also applies to a release PR selected full. No required measurement was moved into an asynchronous
path, and no protected runtime or `.af/` authority was expanded.

## Evidence, metrics and replay

Each class retains the plan, complete nextest inventory, partition, real JUnit, script JUnit when the
script uses unittest, ci-step JSONL, report JSON and a success receipt. Scripts without unittest have
command-level step evidence only; no invented passed testcase represents them. Default and feature
build variants have distinct JUnit identities for reporting. Base and container timings are separate
artifacts. Under a sealed native AF gate, base Python JUnit writes beneath
`AF_GATE_NEXTEST_RUN/base-evidence`; ordinary writable checkouts use `target/base-evidence`. Policy
tests use private temporary directories outside the source. A chmod-sealed, unprivileged regression
executes the Make recording recipes and proves all source bytes remain unchanged.
The timing writer creates the parent of its explicitly configured `AF_CI_METRICS` file, so the base
workflow can record its first formatting command before a fresh checkout has a `target/` directory.
Native gate timings keep their configured external output path, including when its parent is absent.
JSON and GitHub step summaries expose per-class observed counts, executed/passed/failed/
skipped and not-run inventory counts, test-seconds and class command wall time. Inventory omissions,
missing times and missing Python testcase evidence stay explicit; unknown is never a measured zero.
Ignored tests are listed with their policy rather than disappearing from inventory.

Class runner cost (`observed_runner_command_seconds`) is the sum of observed outer class command
intervals only. Each class's `runner_cost_share` divides its outer interval by that class-only sum;
base and container intervals never enter this denominator. The parallel class critical
path is their maximum only when every selected class has a measured interval. It excludes queue time
and separately reported base/container work; builds inside a class interval are included there.
The aggregate also reports the observed command envelope from the earliest measured command start
to the latest finish, including base/container intervals, and names the last finishing command.
This envelope excludes resolver/setup/final-aggregation intervals and is not total workflow
wall time. Always-on functional unittest counts are reported beside separate base costs.
Inner ci-step intervals are retained for attribution, never added again to outer intervals. JUnit's
suite wall and summed testcase seconds remain separate. These are execution costs, not billed runner
minutes. Tests that passed an existing isolated retry retain the nextest gate's established policy;
raw first-run JUnit can still contain the initial failure, and the command/receipt records gate status.

```sh
# Same resolver as CI; event.json has {"pull_request":{"labels":[{"name":"test:functional"},
# {"name":"test:e2e"}]}}. An explicit JSON file avoids shell/Make evaluation of labels.
make test-plan TEST_LABEL_FILE=event.json
make check-classes
make check-fast
make check-full

# Audit the actual compiled inventory without executing it.
cargo nextest list --workspace --locked --message-format json > target/list.json
python3 scripts/test-classes.py inventory target/list.json --output target/partition.json

# Replay an actual historical JUnit, preserving its run/SHA provenance.
python3 scripts/test-classes.py plan --context local --output target/full-plan.json
python3 scripts/test-classes.py report --plan target/full-plan.json --junit target/historical.xml \
  --inventory target/partition.json --source-run 40179845755 --source-sha HISTORICAL_SHA \
  --replay --output target/replay.json
```

Replay JSON contains functional+e2e, fast, full and perf-only scenarios. It estimates a four-thread
**test-only lower bound** per class from observed exclusive test-seconds plus the greater of parallel
seconds divided by four and the longest parallel test. A parallel-class bound takes the maximum.
It does not model builds, scripts, feature variants, containers, queueing or missing tests. Unknown
class timing stays unknown. Historical classification uses the current catalog and nextest exclusive
list; retain those files with the source run if comparing policy revisions. The report lists missing
inventory data and never treats omitted historical tests as zero or promises wall-time savings.

Historical motivation, not measurements of this implementation: the supplied
[main run 40179845755](https://github.com/PakhomovAlexander/afactory/actions/runs/40179845755) took 74m20s
validation; its preflight perf Python step took 734.632s, Rust build 282.642s and nextest wrapper
3356.440s (JUnit wall 3332.480s). Its giant guarded test took 1697.871s exclusive; all 13 exclusive
tests summed to 1938.062s. The e2e/perf-probe group summed to 1275.431 test-seconds, not wall time.
The supplied [PR run 40130989108](https://github.com/PakhomovAlexander/afactory/actions/runs/40130989108)
took 74m48s with that test at 1738.941s. The supplied
[release run 40189598683](https://github.com/PakhomovAlexander/afactory/actions/runs/40189598683) had builds
of 334–490s. These source-run references are supplied task observations, not independently replayed
artifacts in this checkout. No historical JUnit was supplied, so no measured before/after result or
performance score is claimed. The commands above produce replay estimates when actual JUnit is
available; the offline fixture tests exercise all four scenarios without inventing historical data.
