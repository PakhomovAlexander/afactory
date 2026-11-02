- Update the default Task pipelines to Sol (`gpt-6.1-sol`) medium implementation, Terra
  (`gpt-6.1-terra`) high bugs review, Sol (`gpt-6-sol`) high correctness review and Astra
  (`gpt-6-astra`) medium evaluation, all through Codex. Remove advisory/uix dispatch from both
  defaults; verification stays read-only and the local gate and acceptance contracts stay intact
  ([ADR-0253](../docs/adr/0253-the-default-task-pipeline-lives-in-the-catalog.md)).
