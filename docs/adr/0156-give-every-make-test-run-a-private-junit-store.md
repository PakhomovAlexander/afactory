# ADR-0156: Give every `make test` run a private JUnit store

**Status:** accepted (2026-10-20)

## Context

[Issue #219](https://github.com/PakhomovAlexander/afactory/issues/219), a follow-up to #218.
After nextest, `scripts/nextest-gate.py` prints a summary of the JUnit the run wrote. A Task
gate (`AF_GATE_NEXTEST_TARGET` set by `scripts/verify.sh`) already gives each run a private
store in its own run directory. A plain `make test` did not: it used nextest's default store,
so every run in one checkout wrote and read `target/nextest/ci/junit.xml`. Each run removed
that file before nextest started, but no run owned it until it had read it. If run B wrote its
JUnit after run A's nextest exited and before A read it, A printed B's totals as its own. Exit
statuses were never affected. CI uploads `target/nextest/ci/junit.xml`
(`.github/workflows/validate.yml`), and contributors copy it to compare test time.

## Considered options

- **Lock the shared path for the whole run.** Rejected: two runs in one checkout would queue
  for the whole test suite instead of running side by side.
- **Move the default store and change the CI upload path.** Rejected: the workflow and every
  documented `cp target/nextest/ci/junit.xml` would have to follow a path that now changes
  with every run.
- **A private store per run, then publish only a JUnit that exists.** Rejected: a run that
  writes no JUnit and finishes after another run published would leave that run's JUnit at
  the shared path, so CI would upload results that are not the last run's.
- **A private store per run, then settle the shared path under a short lock.** Chosen.

## Decision

Every run of `scripts/nextest-gate.py` runs nextest with a store in a private run directory.
Both branches use the same helper, the one a Task gate already used. Without
`AF_GATE_NEXTEST_TARGET`, the run directory is a fresh `target/nextest-reports/run-*`. The
gate branch is unchanged.

1. A plain run removes `target/nextest/<profile>/junit.xml` when it starts, so a run that
   writes no JUnit never finds an earlier one there.
2. It summarises the JUnit in its own store, so it never reads another run's.
3. When it ends, it settles the shared path. If its store holds a JUnit, it copies that next to
   the shared path and renames the copy over it, so a reader never sees a partial file. If it
   wrote none, or the copy fails, it removes the shared path.
4. Clearing and settling hold an exclusive `flock` on `target/nextest-reports/<profile>.lock`,
   only for those few file operations, never while nextest runs. Runs therefore change the
   shared path one at a time, and the run that settles last decides whether it exists and what
   it holds. The kernel releases the lock if the process dies.
5. It removes its run directory once the shared path is settled. If the copy fails, it warns
   and keeps the directory, naming the JUnit left there.

A failure to clear, summarise, publish or remove is a warning. The step keeps nextest's exit
status.

## Consequences

- Two overlapping `make test` runs in one checkout each print the totals of their own tests,
  passing or failing. `scripts/test-test-time-report.py` holds one run's nextest open while a
  second run goes from start to end and publishes. It then checks each run's totals, exit
  status and step summary, and that the shared path holds the first run's JUnit, or is absent
  when the first run, which finished last, wrote none.
- The `make test` interface, nextest profile selection, the gates, the CI upload path and the
  retained timings are unchanged.
- `make test` now names its store with `--tool-config-file`, as the Task gate already does.
- `target/nextest-reports/` keeps one empty lock file per profile.
