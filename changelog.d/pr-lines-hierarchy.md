- Show PR additions and deletions in one readable Area/Changes/+/−/Share of additions table, with
  bold parents, NBSP-indented crate/main/tests/other children and compact +N/−N counts.
  Deletion-only rows remain visible; shares use parent additions and show — for zero denominators.
  CI repairs the whole report from exact commits; PR-body parsing and help match the new shape.
  File/directory name collisions label only the file `(file)`, `(file2)`, etc., skipping
  literal sibling names while preserving exact counts and JSON identities.
  JSON emits only `af/PrLines@4` with exact added/deleted totals and nested counts. See the
  [crate-report decision](../docs/adr/2026-11-09-split-pr-additions-by-crate-and-code-kind.md).
- Compare addition and deletion bars on one common scale per sibling group; each column uses
  up to twelve cells with eighth blocks, positive slivers and blank zeros, including bold rows.
  Reject empty or overlapping bold wrappers in zero bar cells so CI repairs them to blanks.
