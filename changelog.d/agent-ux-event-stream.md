- `af task events TASK_ID` follows a Task as JSON lines: one `af/task-event@1` document
  (`schemas/task-event-v1.json`) per event of the Task's log, numbered with the Store's own
  gap-free `sequence`, naming what changed, the node and Attempt and a settled Attempt's result
  and exact charge. Once the Task settles it prints exactly one terminal record with the Task's
  `state` (outcome and `next_action`) and exits with the Task's exit code, as `af task wait`
  does. `--after N` prints only the events after sequence N, so a follower that reconnects gets
  the rest; `--timeout S` stops with 124 and names the `--after` that continues. The command
  takes no `--json`, every other command's `--json` stays one document, and its refusals are
  one `af/error@2` line
  ([ADR-0250](docs/adr/0250-follow-a-task-as-a-stream-of-its-store-events.md)).
