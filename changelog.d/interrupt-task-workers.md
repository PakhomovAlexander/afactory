- Ctrl-C (SIGINT) or SIGTERM during `af task run`, `af task start --execute`, `af review run`
  or `af provider doctor` no longer leaves Worker processes running after af exits. af stops
  and reaps every Worker process group, then ends by the same signal, so the shell reports 130
  (SIGINT) or 143 (SIGTERM). The interrupted Attempt is recorded as a failed, cancelled Attempt
  and keeps its charge. The Task is not finished; stderr says to resume it with
  `af task run TASK_ID`. A second Ctrl-C while stopping kills the remaining Worker groups and
  exits at once ([ADR-0129](docs/adr/0129-stop-task-workers-when-af-is-interrupted.md)).
