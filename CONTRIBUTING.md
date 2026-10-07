# Contributing to Afactory

Thanks for helping. This file is the short version of how changes land; `AGENTS.md` and
`CONTEXT.md` hold the domain language and the working agreements in full, and `docs/adr/`
holds every design decision. Read the ADR that covers the area you are touching before you
change it.

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

```sh
make check
```

That is `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`,
`cargo nextest run --profile ci` over every test binary, and the release-selection check
(`scripts/test-release-resolve.py`). CI runs exactly this, so a green local run is a green PR.
Clippy warnings are errors; fix them rather than allowing them.

The gate runs one test per process and schedules tests across every test binary at once
([ADR-0124](docs/adr/0124-run-tests-in-parallel-processes-and-link-them-once.md)).
`TEST_THREADS` bounds how many run at a time: four on a four-core CI runner, half the cores on
a developer machine. Native-provider fixtures spawn several processes per test, so the host CPU
count itself is not a suitable bound. The few tests that assert against a fixed real-time budget
are listed in `.config/nextest.toml` and run alone; add a test there when its assertion depends
on wall-clock, never by raising the budget it asserts.

The runner is the pinned, checksum-verified binary that `scripts/install-nextest.sh` installs
into a temporary tools directory (CI) or `cargo install cargo-nextest --locked` (a developer
machine). `make check TEST_RUNNER=cargo` keeps the sequential libtest path for comparison.

If you touch `crates/review-sandbox`, also run the live probes:

```sh
make review-kernel-container-probes
```

They need a usable Docker daemon and stay outside `make check` because a missing daemon must
fail loudly there, never skip.

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
nextest just wrote (`target/nextest/ci/junit.xml`, or the gate's run directory under
`nextest-reports/` when `scripts/verify.sh` runs it). CI also adds it to the step summary, and
a Task gate's output ends with it. Before nextest starts, the JUnit an earlier run left at that
path is removed, so a run that writes none is never reported from an old one. The report never
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
is deleted with its index entry, and git history keeps it. Links to a deleted ADR, or to an
internal record deleted at GA, are rewritten to point at the superseding ADR or to plain text;
this is the only edit allowed in another accepted ADR's body (ADR-0113 clauses 6 and 8). Look
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
- One topic per PR. Split unrelated fixes even when they are small.
- `make check` passes; say so in the PR template checklist.
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
