# Contributing to Afactory

Thanks for helping. This file is the short version of how changes land; `AGENTS.md` and
`CONTEXT.md` hold the domain language and the working agreements in full, and `docs/adr/`
holds every design decision. Read the ADR that covers the area you are touching before you
change it.

## First: check this machine

```sh
make prereqs
```

It prints one line per prerequisite, `ok` with what it found or `missing` with the command that
installs it: python3, git 2.30 or later, the toolchain `rust-toolchain.toml` pins (through
rustup), cargo-nextest, a C compiler (`cc`), npx (for `markdownlint-cli2`), minisign, and 30 GiB
free on the volume `target/` builds on. It installs and changes nothing,
and exits 0 when everything is present, 1 otherwise (`scripts/prereqs.py`, issue #161).

## Toolchain

`rust-toolchain.toml` pins the toolchain (`1.88.0`, with `clippy` and `rustfmt`); `rustup`
installs it on the first `cargo` invocation, so there is nothing to choose. Edition 2024,
workspace version in `Cargo.toml`. The container probes need Docker; nothing else needs a
daemon.

`af` supports Linux and macOS only. A `compile_error!` at the root of `review-core` and
`review-process` refuses every other target: every crate with platform-specific code is one of
them or depends on `review-core`, so the guard fails the build before any unix-only call does.
Write unix code directly: do not add `#[cfg(unix)]` gates or `#[cfg(not(unix))]` fallbacks.
Where Linux and macOS differ, split on `target_os`.

## Before every pull request

For a code Task, the implementer runs a filtered nextest selection through `--profile ci`
for the tests it touched and repeats that selection after its last edit. The Task gate owns
the full suite. Before a pull request, run the full local check:

```sh
make check
```

That is `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`,
`cargo nextest run --profile ci` over every test binary, and the release-selection check
(`scripts/test-release-resolve.py`), plus preflight and feature-on perf checks.
This repository's Task gate runs `scripts/verify.sh --profile fast` (functional, e2e, uix).
CI uses the selected classes described below; the default PR omits perf. A local pass alone
does not establish the outcome of separate CI/container work.
Clippy warnings are errors; fix them rather than allowing them.

Full `make check` keeps lightweight policy, source-integrity, catalog/schema checks and lint
before the Rust test build. `cargo test --locked --no-run` must succeed before `ci-step.py`
emits its genuine `stage=build` receipt. The ordinary Rust suite then runs, followed by
`perf-python-tests` (`scripts/test-performance.py`) with `stage=tests`, feature-on tests and
release checks. The Python laboratory suite executes tests and belongs under the test deadline;
putting it in preflight consumed the build allowance before Rust compilation even began.
Neither allowance changes. Standalone `make test` still builds and runs only the Rust suite,
with either nextest or `TEST_RUNNER=cargo`. Selected class execution retains its own ordering
and timing evidence. The focused regression in `scripts/test-test-classes.py` exercises the
native wrapper, real Make recipes and receipt writer with command stand-ins, including a
failed build that prevents both Rust and Python test execution.

The gate runs one test per process and schedules tests across every test binary at once
([ADR-0124](docs/adr/0124-run-tests-in-parallel-processes-and-link-them-once.md)).
`TEST_THREADS` bounds how many tests `make test` runs at a time: four on a four-core CI runner, half
the cores on a developer machine. A bare `cargo nextest run` uses the default profile's four
threads. Native-provider fixtures spawn several processes per test, so the host CPU count
itself is not a suitable bound. The few tests that assert against a fixed real-time budget
are listed in `.config/nextest.toml` under the default profile and run alone in both default
and `ci`; add a test there when its assertion depends on wall-clock, never by raising the
budget it asserts.

The runner is the pinned, checksum-verified binary that `scripts/install-nextest.sh` installs
into a temporary tools directory (CI) or `cargo install cargo-nextest --locked` (a developer
machine). `make check TEST_RUNNER=cargo` keeps the sequential libtest path for comparison.

If you touch `crates/review-sandbox`, also run the live probes:

```sh
make review-kernel-container-probes
```

They need a usable Docker daemon and stay outside `make check` because a missing daemon must
fail loudly there, never skip.

## Flaky tests and the isolation rerun

A gate run reruns the tests that failed in its parallel run before it fails
([ADR-0205](docs/adr/0205-rerun-a-failed-test-alone-report-the-gate-and-retry-a-failed-gate-on-the-same-candidate.md)):
each failed test runs again on its own, one at a time, with no other test alive. A test that
passes alone does not fail the step; one that fails both times does, with nextest's exit status.
More than 25 failed tests are not rerun, and reruns stop after 15 minutes; either way the step
fails with the whole list. The step's output ends with what happened:

```text
isolation rerun: 2 failed in the parallel run; each runs again alone
flaky: af::it task_heavy::heavy_review_cannot_erase_stale_claims
failed twice: af::it cli_surface::help_topics_and_command_paths
```

`flaky:` names the tests that failed beside other tests and passed alone, and `failed twice:` the
ones that are broken. A Task check records the same in its check report, `af/TaskCheckReport@1`,
beside its receipt: under `checks.<name>.tests`, `flaky` lists each such test with both outcomes
(`"parallel": "failed", "isolated": "passed"`), `failed` the tests that failed twice,
`test_threads` the parallel run's thread count and `rerun` what the rerun did (`not_needed`,
`isolated`, `over_bound`, `not_run`); `checks.<name>.host` holds the 1-minute load average at
the check's start and end and the CPUs available to af. `af task show TASK_ID` prints
`check kernel flaky: …` and `check kernel failed twice: …` on their own lines, and
`check kernel host: load … at start, … at end, … CPUs, … test threads` beside a check that did
not pass or had flaky tests. A Task whose gate failed ends with
`next: af task run TASK_ID --retry-gate`, which runs that gate again on the same candidate.

A flaky test is still a defect: fix it at its cause and keep its name. The usual causes are a
fixture wall or lease a loaded host exceeds (give a fixture whose subject is not a deadline the
load-safe wall, ADR-0114, or wait on the observable state the test reads instead of sleeping),
and an assertion on a whole document where one field is under test (assert that field; assert
that no Attempt's usage is unknown on its own, so a lease lost under load fails with its own
message). Prove the fix by running the test 20 times in a row under load — with a second
`cargo build --tests` of another worktree running — through the gate's own scheduling:

```sh
make test-repeat NEXTEST_FILTER='test(/::heavy_review_cannot_erase_/)'
# or with any nextest arguments:
python3 scripts/nextest-gate.py --repeat 20 --profile ci --test-threads 7 \
  -E 'package(af) & binary(it) & test(/^task_heavy::/)'
```

`--repeat N` (`NEXTEST_REPEAT` for `make test-repeat`, 20 by default) runs the whole selection N
times and stops at the first run that fails, naming its failed tests; it never reruns anything.

A test whose assertion depends on wall-clock time cannot be made deterministic: split it into
the deterministic part, which keeps the name and runs in parallel, and a part that keeps the
real-time assertions and is listed in the exclusive override of `.config/nextest.toml` with the
reason beside it (ADR-0124). Nothing else runs while it does. Never widen the budget it asserts.

## Portable fixtures

The gate runs on macOS and on Linux hosts with another umask and other coreutils than yours:

- A fixture that asserts a file mode sets that mode itself (`chmod`, `set_permissions`) instead
  of taking it from the umask; a host with umask 0077 creates 0700 directories. A test that
  depends on the umask runs the code under test in a child process with the umask it names.
- A test about a system tool resolves it by its real path (`realpath $(command -v ls)`) and
  accepts every prefix the policy under test lists: `/bin/ls` is `/usr/bin/ls` with GNU
  coreutils and the multicall `/usr/lib/cargo/bin/coreutils` with Rust coreutils.
- A test that builds Rust for real finds the C linker where rustup's host toolchain does: `CC`,
  then the sanitized `PATH`, then `~/.local/bin`.
- The protected harness pin (`python3 -I scripts/performance/cli.py harness-digest --write`)
  ignores the author's own `HOME` and Cargo state below the tree (`.cargo/.package-cache`,
  `.cargo/registry/`) and, outside a Git work tree, what the root `.gitignore` ignores
  (`/target`), so a pin made in a Worker sandbox after a build there equals one made in a
  checkout. Re-pin whenever you change `Cargo.toml`, `Cargo.lock`, a build script or anything
  under `scripts/performance/` or `fixtures/performance/`.
- A test that writes a script and starts it at once writes it through
  `crate::common::executable`, whose single-threaded writer process keeps the file from being
  `Text file busy` under `cargo test`, and installs `af` through `common::install_private_af`.

## Tests and fixtures

- Unit tests live next to the code; integration tests live in `crates/<crate>/tests/`, one
  file per subject (`capture.rs`, `crash_replay.rs`, `container_probes.rs`, …), with shared
  helpers under `tests/support/` or `tests/common/`. In the larger crates the subject files
  sit under `tests/it/` and `tests/it/main.rs` lists them as modules, so the crate links its
  test dependencies once instead of once per file
  ([ADR-0124](docs/adr/0124-run-tests-in-parallel-processes-and-link-them-once.md)); a new
  subject there is a new file plus one `mod` line.
- A test that reproduces a bug goes in first and fails; the fix follows in the same PR.

## Test time

After every nextest run, passing or failing, `make test` prints a summary of the JUnit report
nextest just wrote. Each run writes it into a private run directory under `nextest-reports/`
(below `target/`, or the gate's target when `scripts/verify.sh` runs it), so two `make test`
runs in one checkout, even overlapping ones, each report their own tests. A plain `make test`
then copies its JUnit to `target/nextest/ci/junit.xml`, replacing the file in one rename, and
removes its run directory. Before nextest starts, the JUnit an earlier run left there is
removed, and a run that writes none removes it again when it ends. Runs take turns on that path,
so it holds the JUnit of the run that finished last, or nothing if that run wrote none. CI also
adds the summary to the step summary, and a Task gate's output ends with it. The report never
changes the step's exit status: if the JUnit is missing or unreadable, or its testcases do not
add up to the `tests`, `failures` and `errors` counts nextest declared, you get a warning
instead. Read it top down:

- **Wall (nextest)** and **Tests** are nextest's own run time and test count, exactly as its
  `Summary` line prints them.
- **Test-seconds (summed)** adds up every test's own time. **Achieved parallelism** is that sum
  divided by the wall time: how many tests were running on average. If it is well below
  `TEST_THREADS`, the time goes to the long tail or the exclusive block.
- **Exclusive block** is the summed time of the tests matched by the `test(/.../)` filters of
  the override in `.config/nextest.toml` that takes every test thread. Nothing else runs while
  they do, so the whole block adds to the wall time.
- The duration histogram counts tests per bucket (`<0.1`, `0.1-1`, `1-3`, `3-10`, `10-30`,
  `30-60`, `>=60` seconds; lower edge inclusive), with their seconds and their share of the
  summed test-seconds. The slowest tests (`--top N`, 20 by default) follow with their test
  binary, then any failed tests.

`python3 scripts/test-time-report.py summary JUNIT` prints the same summary for any report;
`--format json` emits it as an `af.test-time-summary/1` document instead.

For a pull request that changes test time, measure before and after on the same machine with
the same `TEST_THREADS`, keep a copy of each run's JUnit, then compare them:

```sh
git switch main && make test && cp target/nextest/ci/junit.xml /tmp/base-1.xml
# ...repeat for /tmp/base-2.xml and /tmp/base-3.xml, then on your branch:
make test && cp target/nextest/ci/junit.xml /tmp/head-1.xml
# ...repeat for /tmp/head-2.xml and /tmp/head-3.xml, then compare with each side's config:
git show main:.config/nextest.toml > /tmp/base-nextest.toml
python3 scripts/test-time-report.py compare \
  --base-nextest-config /tmp/base-nextest.toml --head-nextest-config .config/nextest.toml \
  /tmp/base-*.xml -- /tmp/head-*.xml
```

Each side's exclusive block is computed from its own config, so a change that adds a test to
the exclusive override or splits one out of it is measured with the list each side ran under;
when the two configs differ, the output names both. `--nextest-config PATH` alone still sets the
config of both sides, and each per-side option defaults to it.

Run times vary between runs, so use three runs per side. The comparison takes the median of
each total per side and prints the delta and percent change. It also lists every test whose
median time moved by at least 1 s, plus tests that were added or removed, as a Markdown table.
Paste that table into the pull request description, next to the `af task report` block.

## Design changes and ADRs

A change to a contract, a wire shape, a gate, a budget, a sandbox boundary, or the release
train is a design change and gets an ADR in `docs/adr/`. `docs/adr/README.md` is the
authority on the shape; in short: take the next number after the highest in `docs/adr/`,
name the file `NNNN-kebab-case-title.md` with a short imperative slug, open with a status line
carrying the status and date (`**Status:** accepted (YYYY-MM-DD)` in most records), state the
context, list the considered options with the reason each was rejected, record the decision,
and end with `## Consequences`. Add the record to the index in `docs/adr/README.md`. An
accepted ADR is immutable: a changed decision is a new ADR that names what it supersedes. A
partially superseded ADR gains a status-line note linking the new one, and that status line may
be restated when the new ADR spends the transition wording it carried; a fully superseded ADR
is deleted with its index entry, and git history keeps it, unless the superseding ADR keeps it
with a superseded status line. Links to a deleted ADR, or to an internal record or completed
design note that was deleted, are rewritten to point at the superseding ADR or to plain text;
this is the only edit allowed in another accepted ADR's body (ADR-0113 clauses 6 and 8,
ADR-0145 clause 8).

A change never adds a reader, migration, alias or fallback for state, configuration or flags an
earlier release wrote. A change to a persisted typed shape gives it a new version and changes its
writers, readers, Store validation, JSON Schema and parity tests together; the old version fails
closed (ADR-0145). Look
at `docs/adr/0045-one-release-train-and-a-pin-that-binds-bytes.md` for the shape. Reference
the ADR from the PR and from its change note.

## Commit messages

Imperative subject, under about 72 characters, describing the change rather than the activity.
The history mixes conventional prefixes with a scope (`fix(task): preserve validated Review
result number semantics`, `docs(task): record integrated fixes`, `perf(task): …`, `test: …`)
and plain imperative subjects (`Record Task retry output admission in a new ADR`).
Either is fine; the prefixes are used but not required. Domain terms keep their capitalisation
(`Task`, `Review`, `Snapshot`, `Gate`) as in `CONTEXT.md`.

## Pull requests

- Every change is made through af Tasks: an implementation pipeline makes it and a
  verification pipeline checks it. The pull request description carries the `af task report`
  of those Tasks
  ([ADR-0142](docs/adr/0142-carry-the-af-task-report-in-every-pull-request.md)): run
  `af task report TASK_ID...` from the repository (with the same `--state` if you used one)
  and paste its whole output, both markers included, into the template's `af task report`
  section. It is read-only and names Providers only by kind and model. The `PR report`
  workflow refuses a description without exactly one well-formed block; Dependabot and
  `release/` pull requests are exempt. `python3 scripts/check-pr-report.py --body FILE`
  runs the same check locally.
- The description also states the lines the pull request adds, per directory
  ([ADR-0195](docs/adr/0195-state-the-lines-a-pull-request-adds-per-directory-and-check-them.md)):
  run `python3 scripts/pr-lines.py` from the repository on your branch, with its base fetched
  (`--base REF` for a base other than `origin/main`), and paste its whole output, both markers
  included, into the template's `Lines added` section: a title with the total and a table with
  one row per directory — its figure, and its share as a bar and a percentage. It counts
  `git diff --numstat` additions under the first directory of each path, a file under a `tests`
  directory as `tests`, a root file under its own name, figures rounded past 1000 (`12k`,
  `1.1k`, `463`). The `PR report` workflow renders the block again from the pull request's
  commits and refuses a description whose block is missing or differs, printing the block to
  paste; after a push, run the command again and paste the block when it changed.
  `python3 scripts/check-pr-report.py --body FILE --diff origin/main HEAD` runs the same
  comparison locally.
- One topic per PR. Split unrelated fixes even when they are small.
- Run full `make check` before the PR and say so in the PR template checklist. The local Task
  gate runs the fast profile; CI gate pull requests and releases provide full validation.
- A user-visible change adds its own note, `changelog.d/<topic>.md` (see
  [`changelog.d/README.md`](changelog.d/README.md)), and never edits `CHANGELOG.md`: a note per
  pull request means two open pull requests never conflict on the changelog. The release
  script collects the notes into the release's section and removes them.
- A design change links its ADR.
- Never weaken a contract, a fixture, a gate, a budget, or a sandbox boundary to make a test
  pass. If a test is wrong, say why in the PR; if the boundary is wrong, that is an ADR.
- Keep generated artifacts (fixtures, schemas) in sync in the same PR that changes their
  source.

## What does not belong here

This repository is the kernel and the `af` CLI. Project-specific pipelines, reviewer packages,
prompts, and provider bindings belong in the consuming repository's `.af/` tree; `af onboard`
and `af self` are how a consumer picks them up. A change that only one project needs is a
policy change there, not a code change here.

## Licence

Afactory is licensed under the Apache License, Version 2.0 (`LICENSE`). By contributing you
agree that your contributions are licensed under the same terms; there is no CLA and no
sign-off requirement.

## Security

Vulnerabilities go through private reporting, never a public issue: see `SECURITY.md`.

## Selecting validation classes

`make check` remains full. The local Task gate uses `scripts/verify.sh --profile fast`,
which resolves functional, e2e and uix; CI gate pull requests and releases run full. For
selective development use `make check-fast` (functional, e2e, uix), `make check-full` (all four,
including perf), or `make test-plan TEST_LABEL_FILE=event.json`
then `make check-classes`. The JSON file has the GitHub event shape
`{"pull_request":{"labels":[{"name":"test:functional"},{"name":"test:e2e"}]}}`.
Selective e2e includes live containers and requires Docker. Fmt/lint and policy checks always run.

Ordinary PRs default to functional + e2e + uix; AF gate PRs targeting `af-gate/**` default full. Any positive `test:functional`, `test:e2e`, `test:uix`, `test:perf`
labels replace that default with an explicit allowlist. `no-functional`, `no-e2e`, `no-uix`, `no-perf`
exclude from defaults. Contradictions, unknown reserved labels and empty selections fail. Skipped
classes are reported as NOT RUN. `no-perf` also exempts the separate Performance measurement workflow;
`test:perf` opts into it. Label changes rerun both workflows and cancel earlier PR work. Choose classes
based on the whole change and record the coverage decision for reviewers.

Every integration module/binary and Python test script needs a reviewed entry in
`.config/test-classes.json`; new internal modules of registered unit binaries follow their documented
component default unless overridden. Tests of help/errors/navigation belong to uix; complete workflows
belong to e2e even if they assert output. Keep nextest exclusive scheduling independent of class.

The guarded kernel perf-class test refuses every tampered harness as a baseline with no build.
Its judged comparison and tampered-candidate refusals after a measured baseline are an ignored
manual measurement: run `make perf-kernel-measure` before a pull request changing
`scripts/performance/`, `crates/review-pipeline/examples/perf-probe/`,
`crates/review-pipeline/build.rs`, the experiment runtime or kernel receipts
([ADR-0252](docs/adr/0252-keep-the-kernel-experiment-measurement-out-of-every-gate.md)).

`make release VERSION=X.Y.Z COMPAT='...' PROFILE=fast` opens a release PR with `release:fast`;
the default PROFILE is full. Provision the ten canonical labels with `make test-labels` (dry-run),
then explicitly `make test-labels APPLY=1`; no PR labels are assigned by that setup command.
Release preparation checks the profile label exists before changing source, committing or pushing.
Changing release PR labels directly reruns selected CI; no policy file or metadata commit is required.
When creating the absent tag, the resolver captures labels from the single exact merged release PR
associated with that commit (matching SHA, main base and release/version head). It freezes the source
PR provenance and plan in the annotated tag, bound to commit/version/catalog. First creation captures
then-current labels; every later retry/tag run uses the annotation and ignores moving labels.
`release:full` forces all classes and rejects partial/excluded
selections. Direct unbound tags run full. All selected Linux/macOS gates and packaged binary, archive,
architecture and signature smoke remain mandatory. Fast/custom release notes state their scope.
The post-publication Performance measurement is asynchronous, not a publication gate.

See [the complete class design](docs/test-classes.md) for precedence, catalog policy, retained evidence
and `scripts/test-classes.py report --replay` commands. Real class timing reports keep wall time,
summed test-seconds and runner cost separate; replay scenarios are estimates, not measured savings.
