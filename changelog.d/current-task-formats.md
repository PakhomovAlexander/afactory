- Use one current format for every Task Pipeline (`af.pipeline/6`), compiled graph
  (`af.compiled-task/5`, `af/CompiledTask@3`) and installed kind (`af.task-kind/3`). Project Briefs,
  bounded recovery and structural acceptance retain their explicit capabilities, authority and
  validation. The Pipeline discriminator requires its canonical string form. Retired formats are
  refused without migration; shipped packages and starters use
  the current formats. Task inspection is `af/task-inspection@19`, recorded-plan inspection is
  `af/task-plan-inspection@5`, and catalog contract fixtures use `af.catalog-contract-fixtures/2`.
  See the [consolidation decision](../docs/adr/2026-11-08-consolidate-current-contracts-and-task-review.md).
