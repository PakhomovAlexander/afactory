# `af task report` fixture

The block a pull request description carries
([ADR-0142](../../docs/adr/0142-carry-the-af-task-report-in-every-pull-request.md)), as the real
renderer printed it for a test Store.

- `report.json` is the `af/task-report@1` document `af task report --json` printed for the Store
  of `crates/af/tests/it/task_report_command.rs`'s resumed-Task case: one implementation Task,
  interrupted during its first run and resumed once, with one failed Attempt.
- `report.md` is the Markdown block `af task report` printed for the same Store.

Three suites hold them together. `crates/af/src/task_execution/task_report/tests.rs` renders
`report.json` and requires exactly `report.md`; `crates/review-core/tests/schema_parity/task_report.rs`
validates `report.json` against `schemas/task-report-v1.json`; and `scripts/test-check-pr-report.py`
requires `scripts/check-pr-report.py` to accept `report.md`. A renderer change the fixture does not
follow fails the first; a checker change that refuses the renderer's block fails the last.

To regenerate both files from a fresh test Store, run from the repository root:

```sh
AF_TASK_REPORT_FIXTURE="$PWD/fixtures/task-report" cargo test --workspace --test it \
  task_report_command::a_task_resumed_once_reports_two_runs_and_active_time_below_its_wall_time
```
