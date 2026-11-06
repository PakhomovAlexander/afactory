# Issue #697 native capture investigation

## What I saw

The exact two-attempt assertion can fail when native macOS FSEvents deliver old events during
an otherwise stable second attempt. A focused pass in the command sandbox is insufficient:
that sandbox delivered no native watcher events, even with two-second observer delays.

The added `a_settled_worktree_does_not_replay_a_previous_attempts_events` regression retains the
original two-attempt and settled-content assertions. Its observer delays each pass by two
seconds before performing the first attempt's single edit. This changes test scheduling only;
production's maximum three attempts and 250 ms boundary wait are unchanged.

## Evidence

Temporary tracing of the original implementation, executed natively, recorded:

```text
attempt=1 changed=true  index_equal=true passes_equal=false
attempt=2 changed=true  index_equal=true passes_equal=true
attempt=3 changed=false index_equal=true passes_equal=true
assertion failed: actual attempts 3, expected 2
```

Callbacks included fixture repository/`.git` creation and first-attempt source activity.
The original native diagnostic log's SHA-256 is
`288c3f87ab0d13933c76b63b0c4f98a68691f42c4f90d2845d343767bbbeac93`.
Temporary tracing has been removed.

An experiment retained one monitor across attempts and consumed its complete delivered batch
under the unchanged 250 ms deadline. Its initial native selection passed 26 tests. Under pinned
Rust 1.88.0 the same selection passed 25 and failed the delayed-event regression with three
attempts. This experiment therefore did **not** establish a fix and was reverted, including
its proposed ADR/changelog and associated unit tests. No production consistency checks have
been changed. Delayed delivery remains unresolved; the regression has not been weakened.

After reverting the experiment, the final native Rust 1.88.0 capture selection ran 23 tests:
21 passed and two failed. The delayed-event regression and the existing
`an_untracked_file_created_between_passes_retries_without_a_cas_error` both reported three
attempts instead of two. This confirms the unresolved issue extends to untracked creation.

The attempted complete fast gate used Rust 1.88.0, Python 3.15 and TEST_THREADS=4. It stopped
in functional `test-ci-disk.py` with three failures and three errors: placing TMPDIR below the
checkout made its fake SDK paths overlap protected workspace state. E2e/uix did not run.
This is an environment/setup failure, not a passing gate or evidence about capture correctness.

## Repro

Build with the repository's rustup proxy first on PATH; Homebrew Cargo otherwise selects Rust
1.94.0 despite `rust-toolchain.toml`. Use Python 3.15, a private writable TMPDIR, and native
execution with real FSEvents delivery. Run:

```sh
cargo nextest run --profile ci -p review-source-git \
  -E 'test(capture::a_settled_worktree_does_not_replay_a_previous_attempts_events)' --no-capture
```

A full gate's TMPDIR must also avoid overlap with its protected checkout and build target;
otherwise the CI-disk fixture correctly refuses its fake SDK cleanup before the Rust suite.
The complete kernelguard module was not run; six focused regeneration/refusal cases passed.
Independent bugs/correctness/evaluator acceptance has not been established for this candidate.
