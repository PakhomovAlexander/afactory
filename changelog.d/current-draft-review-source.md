- Consolidate Document and Report drafts into `af/DocumentDraft@3` with explicit source and
  repository citation kinds; Document Tasks still have no repository Snapshot and Report citations
  still resolve against their exact captured Snapshot. Retired draft formats are refused. See the
  [consolidation decision](../docs/adr/2026-11-08-consolidate-current-contracts-and-task-review.md).
- Use `af/TaskReviewSubject@3` with shared borrowed validation, retaining complete changed paths,
  exact base/head identities, bounded readable patches and reviewer assignments. Task source origins
  use `af.task-source-origin/3`, tagged `captured` or `bound`; bound outputs cannot claim a commit.
- Remove obsolete DTO conversions and correct internal Pipeline, plan and Provider admission names
  without changing their wire identities. Release dispatch, retention and storage remain unchanged.
