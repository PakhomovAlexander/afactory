- Native Task Review acceptance now consumes the selected committed successor Round, or a later
  canonical Round carrying its exact history. A Select condition cannot hide successor Findings
  behind an earlier passing Review; embedded acceptance and Store finish/replay enforce the same
  boundary. Campaign Review remains available during the staged port.
- Captured engines use `af.task-engine/3`, Task inspection uses `af/task-inspection@19`, recorded
  plan inspection uses `af/task-plan-inspection@5`, and Experimental execution plans use
  `af/ExperimentExecutionPlan@2` with `af.experiment-execution-plan/2`. Their retired generations
  fail closed. See the [consolidation decision](../docs/adr/2026-11-08-consolidate-current-contracts-and-task-review.md)
  and [implementation evidence](../docs/design/current-contracts-and-task-review.md).
