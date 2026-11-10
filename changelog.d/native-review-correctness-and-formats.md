- Require a complete Review Round over every native Integration commit before Task acceptance,
  including embedded implementation and verification. Keep each Scatter child's exact Diff Subject,
  accept complete sharded Review through the common acceptance path, and deduplicate identical sealed
  edits while retaining original claims and refusing distinct overlaps.
- Version current Task Pipelines as `af.pipeline/6` and compiled graphs as `af.compiled-task/5` in
  `af/CompiledTask@3`, with closed shared schema definitions v4, real compiler parity and refreshed
  package pins and fixtures. Retired formats fail closed, as required by the
  [consolidation decision](../docs/adr/2026-11-08-consolidate-current-contracts-and-task-review.md).
