- A check the kernel stops at its wall now says so everywhere: its check receipt,
  `af/TaskCheckReceipt@2`, records `stopped` with the cause `wall`, the wall and how long it ran;
  the Task ends `check_failed`, never `preflight_failed`; and `af task show`, `af task show
  --checks` (`af/task-checks@3`), `af task report` and the run's stderr say `check NAME stopped
  at its 90 min wall after 88 min` and what to do next. Only a check that did not start says
  `never started`. A Task file may state `acceptance`, typed items each judged by its ID: they
  are captured as `af/TaskAcceptance@1`, carried in the receipt and handed to the evaluator on
  the Pipeline's optional `acceptance` input, and a pass that leaves an item unjudged does not
  verify. `af/TaskCheckReceipt@1` is refused by name: rename it to `@2` in every package and
  re-pin with `af catalog digest`
  ([ADR-0255](docs/adr/0255-say-a-check-stopped-at-its-wall-and-carry-typed-acceptance-in-the-check-receipt.md)).
- Structured acceptance is refused unless an evaluator is held to it: a Pipeline that declares
  the `acceptance` port but hands it to no `verify` node is refused when the plan is compiled, a
  Task of a kind whose domain does not judge items by ID (a review, document, report or
  optimization Task) is refused at plan time, and a passing evaluation over a receipt with items is refused
  unless its evaluator received that record on its own `acceptance` input. Both plan-time
  refusals name the Task file and, under `--json`, are the typed `task_file_invalid` diagnosis
  of its `acceptance` field (`file`, `field`, the item IDs as `value`, and `next_action` the
  `af task plan --file` that plans it again), as `limits.wall_ms` is refused. The performance
  laboratory gives each side of a pair across ADR-0255 the check receipt its own source writes,
  read from that side's source, so `cli_task_start` and `cli_task_run_check` measure both sides
  ([ADR-0255](docs/adr/0255-say-a-check-stopped-at-its-wall-and-carry-typed-acceptance-in-the-check-receipt.md)).
