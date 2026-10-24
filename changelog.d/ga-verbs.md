- The agent loop's verbs
  ([ADR-0205](docs/adr/0205-give-the-agent-loop-its-verbs-and-deliver-a-verified-verification.md)):
  `af task findings ID [--round N]` prints a review round's findings per reviewer — severity,
  confidence, title, location, evidence and fix, grouped by severity — and the evaluator's
  verdict (`af/task-findings@1`); `af task diff ID` prints a candidate's change as `git diff`
  would, touching no worktree (`af/task-diff@1`); `af task materialize ID --into DIR` makes a
  worktree's tracked tree equal to a Task's candidate, refusing uncommitted changes without
  `--force` and an unverified candidate without `--allow-unverified` (`af/task-materialized@1`).
- `af task run ID --detach` runs a Task in the background and returns with its pid and its log,
  `runs/<ID>-<N>.log` in the Store (`af/task-detached@1`); `af task wait ID [--timeout S]` blocks
  until the Task settles and exits with its code (124 on timeout); `af task status ID` prints one
  line now (`af/task-status@1`); `af task cancel ID` stops the run its lease names, by its
  recorded pid only, records who asked, and leaves the Task `abandoned/cancelled/host_cancelled`
  and resumable (`af/task-cancelled@1`). Every run records its process in `runs/<ID>-<N>.json`.
- `af task run` prints one stderr line per node that starts or finishes, with the elapsed time
  and the tokens so far, and a heartbeat every 5 minutes (`--quiet` keeps them back); its last
  stderr line is the state and the next command in text as under `--json`.
- A verified `verification` Task of another Task's candidate delivers that candidate on the
  other Task's source, recording both Tasks (`af/TaskDeliveryProof@1`); `af task deliver`'s
  other refusals name `af task materialize`.
- `af task report IDS… --pr-body [--template FILE]` prints the whole pull request description,
  the report and the `scripts/pr-lines.py` block spliced into the template, which
  `scripts/check-pr-report.py` accepts; `af help agents` and [docs/agents.md](docs/agents.md)
  walk the loop.
