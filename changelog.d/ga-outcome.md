- One Task state behind every rendering: `af task list`, `af task show`, `af task report` and the
  last line of `af task run` print the same `phase/outcome/cause` (`finished/gate_failed/check_failed`),
  in text and under `--json` (`state`), with the one command to run next (`next: af task run ID
  --retry-gate`); `af help task-states` and [docs/tasks.md](docs/tasks.md#task-states) list every
  phase, outcome and cause. A failed gate is `gate_failed`, never `changes_requested`, and
  `af task run` exits 5 for it, 6 for a Provider failure and 7 for an expired deadline
  (`af help exit-codes`). The documents are new versions: `af/task-inspection@12`,
  `af/task-list@3` of `af/task-list-entry@3`, `af/task-report@2`,
  `af/task-collected-inspection@2` and `af/task-selection@2`; the previous ones are refused.
- `af task show ID --checks` (`af/task-checks@1` under `--json`) prints the gate's last check
  receipt with each check's failing and flaky tests and the end of its stderr; `af task list
  --json` rows carry `gate`, the run log names the failing tests after a failed gate, and
  `af task show` names each failed node's cause, first error line and diagnostic artifact.
- `af task list` prints a header row with ID, state, round, tokens, age and the next command's
  verb, and shows a delivery only once there is one.
- A refused plan prints its reason on stderr, and an infeasible one the smallest limits that fit,
  computed from the reserve each verification slot protects (issue #250); `af task plan --fit`
  plans with them and prints what it raised, and `--json` carries
  `planning_diagnostic.required_limits`.
- A Task file without `strategy` takes its kind's (`implement`: `reviewed`, `review`,
  `facts.standard`); planning the same file again shows the existing plan, and `af task plan
  --replace` re-plans an unstarted Task in place.
- A Task's wall clock starts at its first `af task run`, not at `plan`: the preview says
  `deadline starts when you run`, and the run prints the deadline in local time
  (`deadline 17:41:18 +03 (2 h 59 min from now)`).
- Not-found errors name the Task, the Store's path and the Tasks it holds; an unreadable Task file,
  a missing output port and Task state inside the repository name the path, the ports or the
  repository root.
