# CLI help startup correction (#486)

## Evidence and scope

The Task supplied the verified Claw comparison of v0.13.0 and v0.14.0-rc.1:

| Case | v0.13.0 median (ms) | v0.14.0-rc.1 median (ms) |
| --- | ---: | ---: |
| `help` | 4.0796485 | 5.642519 |
| `review_run_help` | 4.1526385 | 6.0810375 |
| `task_help` | 4.4067185 | 6.2178335 |
| `version` | 4.0874015 | 4.089326 |

All 40 pairs per case were present, with no failed samples and healthy shards. These figures
are supplied historical evidence, not measurements made by this Task. CPU profiles identify
command construction/parsing and help rendering as leads; they do not establish causation.
The startup double parse existed in **both** releases. This change does not claim it was newly
introduced or that removing it alone resolves the regression.

## Source explanation

In `crates/af/src/main.rs`, startup previously called `Af::try_parse_from(&argv)` to recognize
`catalog digest`, discarded every other result, called `self_management::maybe_dispatch`, then called
`try_parse_from` again. Each parse constructs the complete derived clap command tree. For a
help flag, clap also renders the requested help into its display error each time. The first
rendered help was thrown away.

Startup now retains that first `Result<Af, clap::Error>`. A borrowed successful catalog-digest
selection still runs before self-management and exits immediately, as
[ADR-0157](adr/0157-print-a-task-package-digest-with-the-pin-recipe.md) requires. Every other
result passes through the same `maybe_dispatch` call before it is consumed. In particular, a
local parse error must not prevent a pinned release from accepting its own command vocabulary.
If dispatch returns, startup consumes the exact parse result rather than repeating the work.

In `crates/af/src/error_document.rs`, `usage` previously tested JSON eligibility before excluding
`DisplayHelp` and `DisplayVersion`. Without `--json`, that eligibility test called
`command_path`, constructing another full clap tree and cloning subtrees while walking it,
even though help could never produce a usage document. With `--json`, the eligibility test
short-circuited and did not construct that extra tree for help.

`usage` now excludes those two display kinds first. Actual errors resolve their command path
once and reuse it for eligibility and document construction. This also removes the repeated
path lookup for `task events` errors without `--json`. The existing missing-subcommand message,
error rendering, `af/error@2` fields, next action and exit code are unchanged. No persisted or
printed shape changes, so there is no schema version change.

Thus ordinary help flags without `--json` go from two parsing/rendering passes plus a usage
command tree to one parsing/rendering pass with no usage tree. Help flags with `--json` go from
two parsing/rendering passes to one. This is a source-level reduction in work, not a measured
latency improvement. The command definitions, help templates and clap renderer are unchanged.
There is no cached or hard-coded help, command pruning or special benchmark path.

Completion handling still precedes parsing. The early auth handoff and calibration controls,
release-dispatch hooks, startup trace lifetime and export, command telemetry, and after-command
hooks remain in the same order. The retained parse result is process-local; it grants no new
authority and adds no filesystem, network or Provider access.

## Correctness coverage and gate cost

Existing coverage already checked scoped help and usage, version text/JSON, static and dynamic
completions (`cli_surface`), help layout (`help_hygiene`), all JSON-capable command paths
(`error_document::tests::every_json_command_reports_a_usage_error_as_one_document`), and the
early digest boundary (`catalog_digest::the_digest_never_dispatches_installs_or_schedules_an_update_check`).
Those contracts were preserved rather than reimplemented.

New coverage:

- `cli_surface::help_flags_preserve_text_exit_code_and_streams_with_json`: root, review run,
  task and task events, both `-h` and `--help`, with and without a following `--json`. It compares
  exact text, exit 0 and empty stderr, including absent required arguments and the JSON-lines
  command. It uses `crate::common::af()` and a private working directory.
- `error_document::tests::events_usage_is_json_without_a_flag_but_help_and_version_are_not_errors`:
  real missing-argument and unknown-flag errors retain command, cause, next action and exit 2
  without `--json`; display-help/version kinds produce no error document with either JSON
  eligibility route. This is in-process.
- `self_managed::dispatch_runs_the_version_a_project_pins_when_the_bytes_match` gains one local
  fake-release invocation: a command rejected by this binary is accepted by the installed pin.
  Existing signed directory-release fixtures supply the pin; no external service is contacted.

The permanent gate addition is two tests, 16 af child launches for the help matrix and one
extra af child launch in an existing dispatch test, plus two in-process clap parses and two
usage-path command trees. There are no new test binaries, fixtures, sleeps, timing thresholds, dependencies or profile overrides.
The existing module classifications cover the additions (uix, functional and e2e respectively).
No shared helper changes, including helpers used by Linux-only tests, are needed. In the
corrected focused run, nextest reported 0.122 s for the new help test and 0.022 s for the new
unit test (0.144 s of test duration). This is a test receipt, not a full gate-wall delta or a
CLI latency benchmark; the dispatch extension adds one child to an existing fixture.

A separate, temporary output check captured 30 exit-code/stdout/stderr records from unchanged
production code, then compares the corrected binary with those records. Its cases are the 16
help combinations above, bare af, both version forms, bare task, two task-events usage errors,
plain/JSON review-run usage errors, five completion scripts and dynamic fish completion.
It uses private HOME/XDG directories, an empty working directory, offline self-management and
no timing measurements. This check adds nothing to future gates. The permanent behavior tests
protect output contracts; they do not assert time or purport to detect the original latency
regression. The removal of redundant calls is established by the source change.

## Local check record

Implementation started at 2026-11-01 18:23:47 MSK. The shell initially selected Homebrew Rust
1.94.0; all builds and Rust checks below explicitly use installed Rust 1.88.0, pinned by
`rust-toolchain.toml`. The private shell HOME cannot initialize rustup and its inherited TMPDIR
refused compiler scratch writes. The check environment selects the existing toolchain/cache
and the sandbox's writable scratch directory explicitly:

```sh
export RUSTUP_HOME=/Users/apakhomov/.rustup
export CARGO_HOME=/Users/apakhomov/.cargo
export PATH="/Users/apakhomov/.cargo/bin:$PATH"
export TMPDIR=/tmp
```

Cargo still builds in this checkout's own `target/`; `CARGO_TARGET_DIR` is never set.

- `rustc --version`: `rustc 1.88.0 (6b00bc388 2025-06-23)`.
- `cargo build --locked --offline -p af`: passed on unchanged production code, for the
  temporary output capture; no benchmark was run.
- `cargo fmt --all`: passed.
- `cargo clippy --workspace --all-targets --offline -- -D warnings`: passed (exit 0).
- `python3 -I scripts/test-classes.py audit`: passed.
- `python3 -I scripts/changelog-notes.py --check`: passed.
- Temporary output comparison: all 30 cases had byte-identical stdout/stderr and identical
  exit codes after rebuilding the corrected binary; no timing was collected.
- `npx --offline --yes markdownlint-cli2@0.22.1 docs/performance-cli-help-486.md
  changelog.d/cli-help-486.md`: markdownlint: not run. The sandbox npm cache has no cached
  response for the pinned package (`ENOTCACHED`); no network fetch was attempted.

Focused Rust checks (installed pinned toolchain and the environment above):

```sh
cargo nextest run --workspace --locked --offline --profile ci \
  -E '(binary(af) & (test(/^cli::tests::/) | test(/^error_document::tests::/))) | (binary_id(af::it) & (test(/^cli_surface::/) | test(/^catalog_digest::/) | test(/^help_hygiene::/) | test(/^self_managed::dispatch_runs_the_version_a_project_pins_when_the_bytes_match$/)))'
```

The initial run reported `68 tests run: 67 passed, 1 failed, 1834 skipped`. The new help
matrix incorrectly rejected any help text containing `af/error@2`; task-events help correctly
mentions that schema in its explanation. The assertion now checks that help is not a JSON
document, while retaining the exact text comparison, exit code and stream assertions.
This was a test-writing error, not a timing flake or a product behavior change.

The corrected selection passed at 2026-11-01 18:33 MSK (exit 0):
`Summary [9.874s] 68 tests run: 68 passed, 1834 skipped`. The skips are tests outside the focused
filter, not exclusions added to the gate. The same focused selection is repeated after the
final edits; the operator report records the last invocation's time and result.

No script behavior is changed, so there are no touched script test suites. No duplicate full
suite or local gate is run. The native AF pipeline supplies `scripts/verify.sh --profile fast`
on the sealed candidate; that receipt is pending at implementation time, not claimed here.

## External performance proof: pending

No benchmark was run on this Mac. No Claw comparison of this candidate is available, and no
latency improvement or resolved regression is claimed. Final delivery and issue resolution
still require the outer operator to retain native verified Performance Lab reports under
Claw's common lock, with the same harness/build environment:

1. Unfixed current base versus the sealed fixed candidate.
2. v0.13.0 versus that same fixed candidate.

Both comparisons select `help`, `review_run_help`, `task_help` and `version`, with nightly
40 paired samples per case. A separate light verification Task must inspect the exact patch
and retained measurement evidence. A partial improvement is not a resolved regression.
The Performance Lab framework, thresholds and fixtures are unchanged.
