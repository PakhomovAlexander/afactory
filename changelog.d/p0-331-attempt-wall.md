- An implementer stopped by its Attempt wall keeps its edits: the kernel seals the tree it left as
  a partial candidate (`af/TaskPartialCandidate@1`), never accepted, verified or delivered on its
  own, and records the end as `af/TaskAttemptWallEnd@1` with the wall and the time the Attempt
  took. The Task ends `exhausted/attempt_wall` (exit 4), not `malformed_reply`; `af task show`,
  its `--json` (`attempt_wall_ends`), `af task report` and the run's stderr say `implementer
  reached its Attempt wall of 30 min after 30 min`, and the next command, `af task run NEW_TASK_ID
  --from TASK`, starts a new Task from those edits. The Attempt is charged and counted as every
  failed Attempt is and spends no recovery allowance. `af task plan` prints each Worker slot's
  Attempt wall beside its model (`slots` under `--json`). Identity probes that proved nothing
  after their retries now read `provider_failed/provider_environment` with `af provider status`
  next, and an account that answered as another reads `identity_changed`. The Task state is
  `task-state-v2`, so the documents that print it are `af/task-inspection@14`, `af/task-list@4`,
  `af/task-list-entry@4`, `af/task-checks@3`, `af/task-collected-inspection@3`,
  `af/task-detached@3`, `af/task-findings@2`, `af/task-report@3`, `af/task-status@2` and
  `af/task-cancelled@2`
  ([ADR-0253](../docs/adr/0253-seal-the-implementers-tree-when-its-attempt-wall-ends-it.md)).
- A wall end survives its writer: the Attempt's reported usage is retained before the wall end is
  written, so a CAS failure then loses no counter, and the wall end's ID is kept in the Store
  before the rest of the Attempt is published, so a run interrupted after it is resumed with
  that Attempt settled `failed` at its wall, its partial tree kept for `--from` and no recovery
  allowance spent; `af task gc --apply` run before that resume keeps the wall end and the partial
  tree it names. `af task report` counts the Attempt under the new failure class
  `attempt_wall`, never `provider_failure`, and its row ends with `next: af task run NEW_TASK_ID
  --from TASK`. The plan preview lists every slot that runs with its Attempt wall, the ninth
  included, the walls beside each Worker in `--tree`, and an optimization experiment's baseline
  and candidate arms with the wall their Worker packages declare (`attempt_wall_ms` under
  `--json`) ([ADR-0253](../docs/adr/0253-seal-the-implementers-tree-when-its-attempt-wall-ends-it.md)).
