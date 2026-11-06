- Packages can opt into Snapshot-bound build/private-install/test evidence beside the unchanged
  Reviewer Result. Evaluators retain the exact evidence and cannot pass failed, unavailable,
  missing or stale testing. Existing pipelines keep their contracts; packages choose their own
  providers/models. This adds no CI, publication or merge authority. See
  [usage](../docs/task-execution/executable-review.md) and
  [decision](../docs/adr/2026-11-06-bind-executable-review-evidence-to-evaluation.md).
