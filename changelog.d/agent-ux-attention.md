- `af task list --attention` groups the Tasks of a Store by what they wait for: approval, a failed
  gate, findings to answer, verified and ready to deliver, and stopped (a Provider, an interrupt,
  a limit or the wall clock ended it). Each Task in those groups is printed with its one next
  command; running Tasks are named on one line and the Tasks that need nothing are counted.
  `--json` prints `af/task-attention@1` (`schemas/task-attention-v1.json`) with all seven groups,
  `needs_you` on each. The groups are read from each Task's state alone and never recorded; `af
  help task-states` and `docs/tasks.md` list them. The browser's Tasks folder opens on the same
  groups under `NEEDS YOU`, with each Task's next command.
