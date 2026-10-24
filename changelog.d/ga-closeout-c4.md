- `af task run --detach` no longer claims a run it did not see start: its document is now
  `af/task-detached@2` with `admitted` and `next_action`, and a run that neither took the Task
  nor admitted its plan within 2 minutes exits 4 with `admitted: false` and `af task status ID`
  next
  ([ADR-0202](docs/adr/0202-give-the-agent-loop-its-verbs-and-deliver-a-verified-verification.md)).
- A second `af task cancel` of one run reports the first request, its requester and time, and
  waits for the same stop instead of signalling again. `af task status` names the run that holds
  the Task's lease and prints the next command on its own indented lines, broken between its
  arguments, within 100 columns (the terminal's width when narrower). `af task run` prints a
  `started` line for every node, the kernel's own (`seal`, `check`, `review.reduce`, `accept`)
  included, before its `finished` or `failed` line.
- `af task report --pr-body` exits 1, naming `--base REF`, when the checkout's
  `scripts/pr-lines.py` cannot print its lines added block, instead of printing a body the pull
  request check refuses; the new `--base REF` is passed to the script. `af task report` warns on
  stderr when two Tasks' candidates add the same `docs/adr/NNNN-*.md` number, naming both.
- `af task findings --round N` and a continued Task's `af/TaskReviewFindings@1` are built by one
  function and carry the same findings; a refused `--round` of a Task without a review round
  ends with the Task's next command.
- A Store gets this release's writer marker in the transaction of its first appended event, never
  at an open: a Store another release wrote and this one refused stays named by that release, and
  a marker that cannot be written fails the append. The summary of Stores several releases wrote
  names one `AF_VERSION=…` command per release instead of the newest for all, within 100 columns.
- The browser's Campaigns pane reads the Stores every 5 seconds while no Round is open, so a new
  Campaign appears without a key; shows a Campaign Store it cannot look at as an error naming the
  path; shows an open Round's own reviewer chips, none before its first Attempt, and a node's
  latest Attempt by its start, not its ID. Two unavailable registered Task Stores are two rows.
  `af review campaigns --state-root R` with no Campaign names
  `af review run --campaign NAME --state R/NAME`, within 100 columns.
