- The gate reruns the tests that failed in its parallel run once each, alone, before it fails: a
  test that passes alone is printed `flaky:` and no longer fails the step; one that fails twice
  does, as before. More than 25 failed tests, or 15 minutes of reruns, fail with the whole list
  ([ADR-0205](docs/adr/0205-rerun-a-failed-test-alone-report-the-gate-and-retry-a-failed-gate-on-the-same-candidate.md)).
  `make test-repeat` (`scripts/nextest-gate.py --repeat N`) runs a selection N times in a row to
  prove a flake fixed.
- Every Task check records `af/TaskCheckReport@1` beside its receipt: the host's 1-minute load
  average at the check's start and end, its CPUs, and what the check wrote to `AF_CHECK_REPORT`
  (`af.check-report/1` lines: its steps by stage, its test threads, its failed and flaky tests).
  `af task show` and `af task run` print where a gate stopped, with its fix (a stale
  protected-harness pin names `python3 -I scripts/performance/cli.py harness-digest --write`),
  the flaky tests and the tests that failed twice, each on its own line.
- `af task run ID --retry-gate` runs a finished Task's failed gate again on the same sealed
  candidate, in a new check Attempt of the same round, under the confirmed plan, with nothing
  re-planned or re-admitted; the Task continues into review when the check passes. A Task that
  ended at a failed gate prints `next: af task run ID --retry-gate`. The Task log gains the event
  `TaskGateRetried@1`; a release before this one cannot read a log that holds it.
- A Task whose source Snapshot pins a Rust toolchain that its `[warm]` checks cannot resolve on
  this machine is refused before its plan is admitted, before any implementer Attempt, naming the
  code policy file and the field, instead of building cold after the implementer ran.
- Two `af task plan` into one Store no longer collide in the content store: every object is
  staged under a temporary name of its own and a lost race is retried (#159).
- The heavy review, starter repair, interrupted repair, source capture, concurrent pipes and TUI
  tests no longer fail under load, the gate's fixtures pass under umask 0077 and with Rust
  coreutils, the real Rust build finds `cc` through `CC` and `~/.local/bin`, the protected
  harness pin made in a Worker sandbox equals a checkout's, and `cargo test` without nextest no
  longer meets `Text file busy` (#121); the reflink cache materialization of #149 has its
  regression test.
