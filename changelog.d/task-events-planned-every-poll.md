- `af task events TASK_ID` refuses a planned Task with no run in progress (exit 1, one
  `af/error@2` line) at every poll, not only the first: a run that was starting when the follower
  began and ended before it took the Task no longer yields a terminal record of the planned
  state with exit 0
  ([ADR-0250](docs/adr/0250-follow-a-task-as-a-stream-of-its-store-events.md)).
