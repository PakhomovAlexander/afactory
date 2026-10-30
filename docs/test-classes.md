# Validation test classes

This design governs repository development and release validation. It changes no kernel runtime,
individual assertion, deadline, or nextest exclusive scheduling rule.
[ADR-0229](adr/0229-select-validation-classes-and-bind-release-profiles-to-tags.md) supersedes the
always-full CI policy in ADR-0107, the unconditional latest-Python lab dependency in ADR-0197,
and the repository operator's prose-only recommendation for `no-perf` in ADR-0196. The AF kernel's
remote gate label authority is unchanged. Local `make check` remains full. The repository's
Task gate selects the fast profile (functional, e2e, uix) through
`scripts/verify.sh --profile fast`
([ADR-0251](adr/0251-run-the-task-gate-on-the-fast-validation-profile.md)); every pull request
defaults to the same fast selection (ADR-0239), and the full check remains in `make check`,
main validation and unbound releases.

## Classes and catalog

Every test has exactly one primary class. Current counts come from the audited current source
modules/scripts and the actual nextest inventory, never from historical run totals:

| Class | Meaning | Examples |
| --- | --- | --- |
| functional | Component and public contract correctness | Typed schemas, config admission, graph compilation |
| e2e | Complete workflows, lifecycle, subprocesses and containment | Task delivery, Campaign rounds, process cancellation, live containers |
| uix | CLI/TUI interaction, output, help, errors and navigation | `cli_surface`, `help_hygiene`, `task_file_errors`, `tui` |
| perf | Measurement, the performance laboratory and expensive performance integration checks | `perf_kernel_measure` refusal checks, `perf_probe`, optimizer and telemetry overhead tests |

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
The catalog also classifies hosted companion jobs: `container-probes` is e2e and
`telemetry-build-smoke` (both build and measurement smoke scripts) and
`telemetry-runtime-smoke` (contained release AF build and real AF experiment paths) are perf.
The new `test_telemetry_corpus.py` module and CLI oracle regressions in `test_runner.py` belong
to the perf laboratory discovery rule; `telemetry_runtime_smoke.py` is the runtime hosted check. Auditing refuses
unclassified hosted validation jobs, and aggregation requires each selected companion to succeed.

`cargo nextest list --workspace --locked --message-format json` supplies actual binaries, tests and
ignored state. The classifier refuses unknown identities, overlapping overrides and empty inventory.
It partitions every identity once, including ignored tests, then generates bounded nextest filters
from the audited inventory. Tests in each root module are grouped only when their selection agrees;
other names stay exact. Explicit singleton overrides classify manual measurement tests as perf,
and the Codex session parser and privacy regressions (`codex_*`) inside the `af` binary's perf
`self_optimizer::` unit module as functional.
Every ignored identity also records its manual/child/container policy; a new ignored test fails
the audit until reviewed. The guarded kernel baseline refusal test is build-free; the judged comparison and
tampered-candidate refusals after a measured baseline are `manual-measurement`: run
`make perf-kernel-measure` before a pull request that changes its protected harness, experiment
runtime or kernel receipts ([ADR-0252](adr/0252-keep-the-kernel-experiment-measurement-out-of-every-gate.md)).
Feature-on trace/calibration checks are additional perf build variants of the
original lint checks, with distinct retained JUnit files. Default-off Rust builds remain unchanged.

Exclusivity is orthogonal: `.config/nextest.toml` remains authoritative. Its real-time tests retain
all threads and their original deadlines within their class job. Classes use separate runner jobs.
The full union includes the former preflight Python checks, performance schemas and harness digest,
ordinary Rust suite and feature-on perf checks. The lab runs once per selected platform on latest
stable Python, with `AF_PERF_REQUIRE_TOMLLIB=1`; it is no longer repeated before Rust validation.

The full native/local gate separates compilation and lightweight preflight from suite execution
([ADR-0231](adr/0231-run-python-performance-tests-after-the-real-build.md)). Preflight retains policy,
source-integrity, catalog, schema and harness checks; formatting and lint remain mandatory.
`cargo test --locked --no-run` must finish successfully and emit its real `stage=build` receipt
before the ordinary Rust suite and the separate `perf-python-tests` target run. The latter runs
`scripts/test-performance.py` with `stage=tests`, followed by feature-on tests and release checks.
This places the laboratory suite under the existing test allowance instead of exhausting the build
allowance before compilation. The fast Task gate also records this shared build receipt before its
first class receipt, ending the build deadline before class tests begin. No test selection,
assertion or deadline changes.
Standalone `make test` retains both Rust runner modes. The selected class entry and its timing
attribution remain unchanged; the phase repair applies to full `make check` and the fast Task gate.

Ignored tests retain the previous policy. Benchmarks, child-process helpers and namespace-only probes
are inventoried as ignored, never silently promoted or discarded. Live container probes are an explicit
e2e job on Linux; missing Docker fails it. `make review-kernel-container-probes` runs exactly the three
existing probe selections, including their ignored tests. They remain outside default local
`make check`, as before; selective e2e execution includes them and requires a live daemon.

## Quarantine

A test shown flaky in the gates leaves every plan until it is fixed
([ADR-0268](adr/0268-quarantine-flaky-tests-on-gate-evidence-and-confirm-them-on-claw.md)).
`.config/test-quarantine.json` (`af.test-quarantine/1`) lists it under `tests` by its outer
nextest identity, in the form the catalog's `exact` map uses:

```json
{"schema": "af.test-quarantine/1", "tests": {
  "af::it capture::a_settled_worktree_is_admitted_on_retry": {
    "state": "quarantined", "since": "2026-10-29", "expires": "2026-11-12",
    "evidence": ["TASK_ID_1", "TASK_ID_2"], "issue": 466}}}
```

`state` is `quarantined` (taken out on gate evidence) or `flaky` (confirmed on the Claw VM);
`expires` is after `since` and at most 14 days after it; `evidence` names the af Tasks whose
gates listed the test as flaky; `issue` is optional. The file ships empty;
`schemas/test-quarantine-v1.json` is its schema, checked for parity by `audit`.

A listed identity classifies as `quarantine`, ahead of every catalog rule. `quarantine` is not a
validation class: no plan selects it, not the fast profile, not `release:full`, not a label or
custom plan. The test still compiles and stays in the inventory; the generated filters leave it
out and keep its siblings, and the class report counts it in no class row or timing estimate.
With the file empty every plan selects what the catalog alone selects. The full gate's
unplanned nextest run (`make test`, reached by `make check` and `scripts/verify.sh --profile
full`) starts through `scripts/test-classes.py nextest-gate`, which adds
`-E 'not (<the quarantined identities>)'` only when the file lists one. `make test-repeat`, a
direct `cargo nextest run` and `TEST_RUNNER=cargo` still run it.

Quarantine covers the default-feature inventory only: an identity the feature builds' fixed
selectors reach (`review-core` `perf_`, `review-pipeline::perf_probe` `calibration_build`), even
one only a feature build compiles, is refused. Fix the test, or exclude it from its feature
selector by hand.

`python3 scripts/test-classes.py audit` (the `test-catalog` step of `make check`), every plan and
every class run refuse, naming the entry and the fix: a file of another shape, an unknown or
missing field, a bad state or date, a window over 14 days, empty evidence, an entry whose
`expires` is before today, more than 20 entries, an `#[ignore]` test, an identity a feature
selector reaches or an identity no catalog rule classifies; at partition time, an identity absent from the compiled inventory or ignored
there. `AF_TEST_QUARANTINE_TODAY=YYYY-MM-DD` replaces the clock for the expiry guard.

Entries are reviewed data, changed by pull request. A test is added when the gates of at least 2
distinct Tasks within 7 days list it as flaky, then confirmed by 50 runs under load on Claw: a
failure makes it `flaky` with an issue for its fix, 50 passes remove the entry, except that an
entry with only macOS evidence stays `quarantined` until it is fixed or expires. A fix is its own
change and removes the entry. The confirmation script is not built yet; until it is, the owner
confirms and removes entries by hand.

`scripts/test-quarantine.py scan` proposes the entries:

```sh
python3 scripts/test-quarantine.py scan --state STORE_DIR             # or --list FILE
python3 scripts/test-quarantine.py scan --list tasks.json --write    # add them
```

It reads one or more `af task list --json` documents (`af/task-list@3` or `af/task-list@4`):
`--list FILE` reads one, `--state DIR` runs `af task list --json --state DIR` (the `af` on `PATH`,
or `--af PATH`); both repeat, and a Task listed twice counts once. Each row's `task_id`,
`updated_unix_ms` and `gate.flaky` are read; a row without `gate` adds nothing. A flaky name is
evidence only when it resolves to exactly one identity of the default-feature nextest inventory
(`--inventory FILE`, else `cargo nextest list --workspace --locked --message-format json`): as
that identity exactly, or as a bare test name that one identity carries (af 0.13.0's text
fallback drops the binary). Every other name is rejected with its reason: `not a test` (no such
identity, an `#[ignore]` test, or one a feature build's fixed selector reaches) or
`ambiguous: <identities>`. The fixture names that `crates/af/tests/it/task_gate.rs` prints, and
0.13.0 recorded as flaky (`gate-store-regression`, `fails_when_crowded`, `af::it`, `many_0`),
never resolve.

An identity flaky in at least 2 distinct Tasks whose `updated_unix_ms` falls within the 7 days
ending at the end of today (UTC) is proposed as `quarantined`, `since` today, `expires` today
plus 14 days, its `evidence` the sorted Task IDs (at most 10), without `issue`. An identity already
in the file keeps its values; `--write` writes the whole file in the shipped format (two-space
indent, sorted keys), so an entry already in that format keeps its bytes and a hand-reformatted one
is normalized. Proposals rank by evidence count, then identity; those past the
20-entry cap are not added and are named. Without `--write` the scan prints what it would add,
the over-cap identities and the rejected names, and changes nothing; `--write` adds them through
every guard above, or refuses and writes nothing, and runs those guards even with nothing to add; `--json` prints the same facts as one
`af.test-quarantine-scan/1` document. Today is `--today YYYY-MM-DD`, else
`AF_TEST_QUARANTINE_TODAY`, else the clock. The exit code is 0 with or without proposals, 1 on bad
input or a guard refusal and 2 on a usage error. `scripts/test-test-quarantine.py` (run by `make check` and the base
check) proves it on test-local lists and inventories.

## Selection and precedence

Labels are case-sensitive data, passed through JSON or quoted arguments, never evaluated as shell.
`test:*`, `no-*`, and `release:*` are reserved namespaces: unknown names in them fail validation.
Other labels have no selection effect. Both label addition and removal rerun validation and replace
older PR runs through their existing concurrency group.

1. Reject any `test:CLASS` with `no-CLASS`, and reject `release:fast` with `release:full`.
2. Every PR defaults to functional + e2e + uix, including AF gate PRs targeting `af-gate/**`
   and release PRs named `release/vX.Y.Z`. Local `make check`, ordinary main and releases
   without a matching merged release PR run all four. A tagged release with a matching PR
   inherits that PR's selection. `release:fast` explicitly selects the fast default.
3. Any positive labels form an explicit allowlist replacing those defaults:
   `test:functional`, `test:e2e`, `test:uix`, `test:perf`. With no positive label, use the default.
4. Subtract `no-functional`, `no-e2e`, `no-uix`, `no-perf`. An empty result fails.
5. `release:full` forces all four and refuses any negative or partial positive allowlist. Explicit
   positives naming all four are allowed. `release:fast` permits a custom positive allowlist or
   further exclusions; the retained profile is then named custom unless its union equals fast/full.

| Labels | PR selection | Profile |
| --- | --- | --- |
| none (any PR) | functional, e2e, uix | fast |
| `test:functional`, `test:e2e` | functional, e2e | custom |
| `test:perf` | perf only | custom |
| `no-uix` | functional, e2e | custom |
| `no-perf` | functional, e2e, uix | fast |
| `release:full` | all four | full |
| `release:full`, `no-perf` | refused | none |
| `test:perf`, `no-perf` | refused | none |

Fmt, ordinary and diagnostic-feature clippy, source/catalog/selection validation, release-resolution,
change-note, quarantine scan and pin policy checks, and the binary version smoke are always-on base work. Their costs are retained
separately; a perf-only plan still performs this work. Release archive, signature, architecture and
actual packaged-binary onboarding/planning smoke always run for every shipped target.

`validation / lint` and `validation / check (ubuntu-latest)` retain their exact branch-protection and
Remote Check names. They are honest aggregation jobs with `always()`. They require resolver and base
success, successful selected class matrix, exact SHA/catalog/plan receipts for every selected platform
and class, live containers when e2e is selected, and actual telemetry build/measurement and real AF runtime smoke success when
perf is selected. The smoke job uses the same resolver output; fast/no-perf skips it explicitly. Missing outputs, skipped selected work, failure
and cancellation fail. Class exclusion appears as **NOT RUN**, with its reason, never as a passed test.
An empty or invalid plan cannot form a self-approving green gate. Releases also aggregate macOS.

## Release binding

`make release VERSION=X.Y.Z COMPAT='...'` passes `--profile fast` to `release.sh` by default;
`PROFILE=full` opts into full validation. Calling `scripts/release.sh` without `--profile` also
selects fast. The script applies `release:fast` or `release:full` to the release PR.
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
No match defaults full. An unlabeled matching release PR selects fast, just as its PR checks did.
Multiple exact matches, API errors, malformed responses, pagination overflow
and invalid label selections fail before creating/pushing a tag.

Before pushing, the resolver freezes `af.release-tests/2` JSON in the annotated tag: exact SHA,
version, source PR repository/number/merge SHA/base/head/merge time (or null), captured labels and
complete resolved plan including catalog digest. The pushed remote tag object must equal the local
object. Competing creators never force or replace a ref. Subsequent main retries and tag-triggered
runs read this annotation and never query moving labels. Malformed/stale bindings, changed catalog,
wrong commit, moved local tag or tag/version mismatch fail closed. The same plan drives Linux/macOS.
Labels are captured at first tag creation, not historically at the merge instant: a label change
between merge and first creation affects that initial capture. Once the annotation exists, later
label changes and even API outages cannot change its selection or block retries.

Plans use `af.test-plan/2`, receipts use `af.test-class-receipt/2`, and tag bindings use
`af.release-tests/2`. The `release_pr` context requires exact merged PR provenance in the binding;
the `release` context without provenance requires the full unlabeled plan. The generated v2
schemas and parity checks cover this added context. Earlier contract versions fail closed under
ADR-0145 rather than being treated as unbound manual tags.

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

The separate `performance-telemetry.yml` producer retains its PR, schedule, release and dispatch
semantics, source bindings, lifecycle fencing and deadlines. It remains advisory and independent
of PR/release class selection. Selecting perf requires validation of that subsystem and its
contained build/measurement smoke; it does not change the producer's execution policy or wait
for scheduled/advisory measurements.

## Evidence, metrics and replay

Each class retains the plan, complete nextest inventory, partition, real JUnit, script JUnit when the
script uses unittest, ci-step JSONL, report JSON and a success receipt. Scripts without unittest have
command-level step evidence only; no invented passed testcase represents them. Default and feature
build variants have distinct JUnit identities for reporting. Base, container and Linux perf smoke timings are separate artifacts. The perf smoke commands
contribute to the observed command envelope, with their cost reported separately from class shares. The gate-owner preflight is mandatory in both the base check and each selected class runner.
`verify.sh` retains the native ownership wrapper; `verify-body.sh` owns the complete gate body.
With no arguments or `--profile full` it runs full `make check`; `--profile fast` writes
a gate-context `release:fast` plan into the private run directory, then runs `test-build` before
`check-classes`.
Explicit `--test-plan PLAN.json` selects `check-classes` as before, keeping its evidence
beneath the existing private gate output.
Native `execute` explicitly passes `--build-scope plan`: every selected variant compiles before
the first successful build-stage receipt. Independent hosted `run` defaults to
`--build-scope class`: functional, e2e and uix compile the default workspace inventory only,
while perf also compiles both feature variants before its build receipt. Both scopes retain
the exact original plan and full inventory audit; a required build failure prevents suite
execution and successful build/class receipts ([ADR-0236](adr/0236-scope-class-builds-to-their-execution.md)).
Under a sealed native AF gate, base Python JUnit writes beneath
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
base, container and perf smoke intervals never enter this denominator. The parallel class critical
path is their maximum only when every selected class has a measured interval. It excludes queue time
and separately reported base/container work; builds inside a class interval are included there.
The aggregate also reports the observed command envelope from the earliest measured command start
to the latest finish, including base/container/perf smoke intervals, and names the last finishing command.
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

## Hosted release validation proof

`Release validation proof` (`.github/workflows/release-validation-proof.yml`) exercises the
existing reusable validator with `release: true`, without running the publishing workflow.
It changes no production default. Dispatch it with the explicit `profile` choice `fast` or
`full`; fast selects functional/e2e/uix and reports perf **NOT RUN**, while full selects all
four classes. Both choices run the selected classes and mandatory base checks on Linux and
macOS. The validator retains its live Linux container probes, selected perf companions,
artifact admission and fail-closed aggregates.

Before merge, the workflow accepts only `pull_request` events whose changed paths include
this proof workflow itself, with exactly one explicit `release:fast` or `release:full` label.
Other PR paths alone never trigger it. Other `test:*` and `no-*` labels still govern ordinary
PR validation; they cannot reduce the proof's chosen profile. Conflicting profile labels fail.
Label events reselect a proof plan from that event's labels; a retry uses the original event
payload. To prove a different profile, change the label or dispatch a new run. No
`pull_request_target`, credentials persisted by checkout, inherited secrets, write permission,
release signing or publication job is used.

The operator starts the live CI runs. After the workflow is available on the default branch,
for example, use the Actions UI or dispatch each profile against the intended ref:

```sh
gh workflow run release-validation-proof.yml --ref BRANCH -f profile=fast
gh workflow run release-validation-proof.yml --ref BRANCH -f profile=full
```

Inspect the proof summary and `release-validation-proof-ATTEMPT` artifact: `plan.json` binds
the exact checked-out `github.sha` (the merge SHA on PR events), workspace version and catalog
digest using the production class resolver. The proof refuses a different checkout SHA.
`local-binding.log` and `local-binding-results.json` retain the existing real release resolver
fixtures, including changed labels on retries, frozen tag SHA/version/catalog refusals and
full fallback for manual/unbound tags. These fixtures use disposable local Git repositories,
local bare remotes and a fixture `gh` API; simulated commits/tags never touch GitHub refs.
Their timing is explicitly a **local binding simulation executed in CI**, not release
publication. Scratch repositories are removed after each fixture.

Require the proof result and reusable validation to succeed. Inspect the usual
`validation-plan-ATTEMPT`, `validation-base-OS-ATTEMPT`, `validation-class-OS-CLASS-ATTEMPT`,
container and aggregate summary artifacts for each selected platform/class; selected-class
JUnit and step timings remain separate from base/build and companion costs. A skipped,
failed or missing selected result is not a proof pass. Compare fast/full observations only
for matching SHA/version/catalog and equivalent runner conditions. Omitted perf work is
NOT RUN; this workflow provides no release publication timing or verified savings claim.
