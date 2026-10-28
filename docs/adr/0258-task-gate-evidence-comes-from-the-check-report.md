# ADR-0258: Task gate evidence comes from the check report

**Status:** proposed (2026-10-28).

## Context

A Task's gate evidence is what `af task show`, `af task list` and `af task report` say about its
last check receipt: `gate.failed_tests` and `gate.flaky` (`TaskGateV1`), each check's failing and
flaky tests and the step that stopped it in `af task show --checks`, the cause
`check_flaky_then_failed` or `preflight_failed`, and the `check NAME: failed: N failing tests`,
`FAIL name` and `flaky:` lines. Until now all of it came from a text scan of the check's whole
stdout and stderr (`review_core::task::state::failing_tests` and `stopped_step`): every
`FAIL`/`TIMEOUT`/`SIGABRT`/`SIGSEGV`/`SIGKILL [..] name` line, every `FLAKY` and `flaky:` line,
every `test X ... FAILED` line, and the first `[label] 1.2s, exit N` line with N not 0.

This repository's gate runs the preflight `scripts/test-nextest-gate.py`, which prints the
output of its own fixture gates into the check's output: their deliberately failing tests
(`FAIL [   0.007s] ( 5/30) gate-store-regression many_10`), their `flaky:` and `failed twice:`
lines and their step lines (`[test] 0.326s, exit 100`). A kernel check of 28 October 2026 that
passed printed 58 such `FAIL` lines beside the real suite's one flaky test; `af task show
--json` reported 29 fixture names in `gate.failed_tests` of a passed gate and the fixture in
`gate.flaky`, and a failed gate was reported `check_flaky_then_failed` with those names (#346).

The authoritative account existed and was refused. Each check writes `af.check-report/1` lines
to `$AF_CHECK_REPORT` (ADR-0205): `scripts/ci-step.py` one `step` record per step with its
stage, `scripts/nextest-gate.py` one `tests` record per nextest run. The preflight gives its
fixtures a private report file, so their records never reach the Task's. But since the gate
runs on the fast profile (ADR-0251), `scripts/test-classes.py` runs `scripts/nextest-gate.py`
once per class (`rust-functional`, `rust-e2e`, `rust-uix`) with the Task's report file, so the
file holds several `tests` records, and `parse_check_report` refused the whole report (`line 26
is a second tests record`): every kernel check's `af/TaskCheckReport@1` was `refused`, with no
steps and no tests, and `af task show` printed that refusal on a passed gate.

## Decision

A check writes one `tests` record per test run, and af folds them into one. The gate's failed
and flaky tests and the step that stopped a check come from its report when the check writes
one, and from the text scan of its output only for a check without one (absent or refused).

1. **The fold.** `parse_check_report` reads every `tests` record, checks each one alone against
   `CheckTestsV1::validate` (a record that breaks it refuses the whole report, naming its line),
   and folds them in the order the check wrote them (`fold_tests` in
   `crates/review-core/src/task/check_report.rs`):
   - `test_threads` is the first record's;
   - `failed` is the ordered union of the runs' names, each once, the first
     `MAX_NAMED_FAILED_TESTS` (100);
   - `failed_count` is the number of distinct failed tests:
     - when every run names all of its failures (each run's `failed_count` below
       `MAX_NAMED_FAILED_TESTS`), it is the size of that union: the names are complete, so a
       repeated name, inside one run or across runs, is one test;
     - when any run is capped (its `failed_count` at or above `MAX_NAMED_FAILED_TESTS`, so it
       counts failures it does not name), it is the checked sum of the runs' counts less one for
       each name a later run repeats from an earlier run; a name repeated inside one run is
       deduplicated in `failed` but stays counted, because the capped run's unnamed failures
       make the true distinct count unknowable and the fold never undercounts. A sum that does
       not fit `failed_count` refuses the whole report, naming the line of the record that
       overflowed.

     In practice a name never repeats across the runs of one gate, because each run is one test
     class and a test belongs to one class; the rule exists so the fold is total;
   - `flaky` is the records' entries in order, each name once, the first `MAX_FLAKY_TESTS` (25);
   - `rerun` is the strongest of the records', `over_bound` > `isolated` > `not_run` >
     `not_needed`.

   This is what `CheckTestsV1` already means: `failed_count` is "every test that failed and was
   not shown flaky" and `failed` "the first of those, by name", so the count and the names
   describe one set, and the folded set is the union of the runs' sets.

   A report with zero `tests` records keeps `tests: None`; one record that names each failed
   test once, as nextest does, is admitted as it was written. Every other refusal stays: the schema,
   an unknown record or field, the line and byte bounds, bytes that are not UTF-8, and a `step`
   that fails validation.

   The folded run holds `CheckTestsV1::validate`, unchanged; the report is still validated again
   after the fold:
   - `test_threads` is a valid record's.
   - Every name is a valid record's name, so it is bounded and printable, and every flaky entry
     failed in parallel and passed alone.
   - `failed` holds at most 100 names and `flaky` at most 25, by the truncation.
   - `failed.len() <= failed_count`: in the first branch the count is the union's size and
     `failed` is the union, truncated. In the second, each record has
     `failed.len() <= failed_count`, so its count is at least its new names plus the names it
     repeats from earlier records; less those repeats, it adds at least as many to the count as
     it adds new names, and the union's size never exceeds the summed counts less the repeats.
   - `failed_count >= 100` or `failed.len() == failed_count`: in the first branch the count
     equals the union's size by construction, so below the bound `failed` is all of the union
     and `failed.len() == failed_count`, and a union of 100 names or more (records below the
     bound may together name more) is a count at or above the bound. In the second branch the
     count stays at or above the largest record's count, which is at or above the bound: the
     repeats a record subtracts are names earlier records added, each counted there, so the
     cross-run repeats subtracted never exceed the other records' names, and only
     `failed.len() <= failed_count` applies.
   - `rerun`: `not_needed` wins only when every record is `not_needed`, so every count is 0 and
     no record has a flaky test; `isolated` or `over_bound` wins only from a record that has a
     failure or (for `isolated`) a flaky test, and a failure in any record leaves
     `failed_count > 0` and a flaky entry survives the fold; `not_run` wins only when no record is
     `isolated` or `over_bound`, so no record has a flaky test.
2. **Report first.** For a check whose run in the receipt's `af/TaskCheckReport@1` (recorded by
   the same check Attempt, naming the same receipt) is `admitted`, its failed tests are
   `tests.failed`, its flaky tests the names of `tests.flaky` (none when the report has no
   `tests`), and its stopped step the report's first step that did not exit 0, with its label,
   exit code and stage. `gate_of` unions them across checks as before. A failed check that named
   no test and stopped at a reported step whose stage is before `tests` (`preflight`, `lint`,
   `build`) is `preflight_failed`, whatever the step's label; one that stopped at a `tests` or
   `release` step is not.
3. **Text fallback.** A check with no admitted report, `absent` or `refused`, keeps the text scan
   of its stdout and stderr exactly as before, with the label rule for its stopped step (a
   label that does not start with `test` stopped it before its tests).
4. **The source, in one word.** Where `af task show --checks` prints a check's evidence (a stopped
   step, failing tests or flaky tests), the check's line ends with `evidence: report` or
   `evidence: output`. No printed or persisted shape changes: `af/task-checks@3`,
   `af/task-inspection@14`, `af/TaskCheckReport@1`, `af.check-report/1` and
   `af/TaskCheckReceipt@2` keep every field and bound.
5. A check's status still comes from its exit code and its receipt, never from its report.

## Consequences

- ADR-0205 is unchanged: the preflight keeps printing its fixture gates' output as diagnostics,
  the isolation rerun and its bound are untouched, and `scripts/nextest-gate.py`,
  `scripts/ci-step.py` and `scripts/test-classes.py` write what they wrote. A passed kernel check
  now has an admitted report again, and `af task show` names its real flaky tests, not the
  preflight's fixtures.
- A gate on a project without `scripts/ci-step.py`, one that writes no report, keeps the text
  evidence it had. A check that writes a report with steps and no `tests` record names no test:
  its report is its account.
- The flaky bound holds across runs: a gate reruns at most 25 tests alone per run, so several
  runs may report more than 25 flaky tests together, and the folded report names the first 25
  in the order the runs wrote them.
- A stop the report places in `preflight`, `lint` or `build` is now `preflight_failed` where the
  text scan read `check_failed` because the gate printed no step log; the next command is the
  same for every gate cause.
