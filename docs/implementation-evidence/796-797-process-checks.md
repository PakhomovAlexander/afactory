# Predecessor process and terminal checks

Historical predecessor evidence, preserved including failed results. Fresh follow-up results are
recorded in [the bounded correction record](796-797-followup.md).

## What I saw

The unchanged native E2E class completed 1,456 parallel Rust tests in 1,045.186s:
1,444 passed and 12 failed. Its ordinary isolation reruns left ten persistent failures;
the class runner exits 1 and its Rust nextest subprocess exits 100. The five new feature cases
all pass. No process/recovery/telemetry assertions, test budgets or sandbox boundaries were changed.

## Evidence

Persistent failures, each repeated in isolation:

- `af::it common::recorded::tests::a_recorded_process_is_only_watched_and_its_holder_ends_it`
- `af::it common::recorded::tests::a_marker_names_a_process_that_started_before_it_was_written`
- `af::it project_survey::a_spill_a_killed_refresh_left_is_swept_by_the_next_while_a_live_refreshs_is_kept`
- `af::it task_recovery::a_killed_run_is_abandoned_then_resumed_with_one_chargeable_and_one_abandoned_attempt`
- `af::it task_recovery::a_node_past_its_recovery_allowance_cannot_continue_and_says_why_on_line_one`
- `af::it telemetry_runs::telemetry_tail_refuses_a_second_store_that_appears_while_following`
- `af::it telemetry_runs::telemetry_tail_refuses_a_segment_recovered_as_corrupt_underneath`
- `af::it telemetry_runs::telemetry_tail_refuses_an_unreadable_sealed_segment`
- `af::it telemetry_runs::telemetry_tail_refuses_interior_corruption_of_a_live_segment`
- `af-process::cancellation cancellation_retains_prefixes_and_stops_running_stdin_and_each_held_drain`

Observed diagnostics include `process ... ended before it was recorded`, `the marker's writer runs`,
`the Worker was recorded`, `descendant must be live at cancellation: stdin`, and
`PermissionDenied: Operation not permitted` at `telemetry_runs.rs:2706` while spawning `ps`.
The two `common::recorded` cases are also reported `FAIL + LEAK` by nextest.

A separate owned-child-only diagnostic attempted `/bin/ps -o stat= -p CHILD_PID` and got
`OSError(errno=1, "Operation not permitted: '/bin/ps'")`. The diagnostic's owned sleep child was
killed and reaped in its `finally` block. This establishes that ps is unavailable here; it does
not establish every failed case's full cause.

The runner reported two cleared isolation cases: the storage-budget Scenario/Ant case and the
pending-attempt report-refresh case. The first was a fixture parity omission in this change:
a generation-6 daily Ant lacked `kind: daily`. Its fixture was corrected without changing any
storage assertions, and the focused rerun passes (exit 0, one test, 2.18s). It is not described as
a spontaneous flaky pass; its source was repaired before the isolation rebuild.

The UIX class was interrupted through its original execution session (exit 130). It logged
65 completed Rust tests (64 pass, one help-width failure), then four TUI cases remained active
beyond 240s and prevented progress:

- `tui::a_command_that_ends_at_once_still_shows_its_exit`
- `tui::a_failing_command_line_shows_its_non_zero_exit`
- `tui::bare_af_in_a_repository_opens_the_project_scope`
- `tui::bare_af_outside_a_repository_opens_the_user_scope`

The new help-text line-width defect was corrected by wrapping the documentation and shortening the
topic title. The unchanged `help_hygiene::no_help_line_is_wider_than_100_columns` then passes
(exit 0, one test, 3.08s). No remaining UIX cases are claimed as passes. A default-sandbox direct
signal to the separate runner was denied (EPERM); the execution session's interrupt stopped it.
A subsequent query for processes holding that invocation's private log returned none.

## Repro

Use the unchanged fast plan, pinned Rust 1.88.0, four threads, private writable HOME/XDG roots,
private offline Cargo cache and checkout-owned target. In this captured worker:

```sh
python3 -I scripts/test-classes.py run --plan target/nextest-reports/run-YdyjSXR3/test-plan.json --class e2e --output target/feature-e2e --threads 4 --build-scope plan
python3 -I scripts/test-classes.py run --plan target/nextest-reports/run-YdyjSXR3/test-plan.json --class uix --output target/feature-uix --threads 4 --build-scope plan
cargo test --offline --locked -p af --test it action_only_scenario_start_resume_and_ant_tick_install_and_sweep_the_storage_budget
cargo test --offline --locked -p af --test it no_help_line_is_wider_than_100_columns
```

The plan comes from the original unmodified `scripts/verify.sh --profile fast` run. The class
continuations received `AF_WORKSPACE_ROOT`, `AF_GATE_NEXTEST_TARGET` and separate
`AF_GATE_NEXTEST_RUN` directories. No exclusion, quarantine, test-budget increase or authority
change was introduced. Checks requiring unavailable process/terminal capabilities remain failed
or incomplete. No external issue was filed under this Task's no-external-messages instruction.
