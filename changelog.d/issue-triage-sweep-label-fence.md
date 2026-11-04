- Persist each sweep label intent and its exact desired labels before clearing canonical authority
  or writing to GitHub. Unknown and partial DELETE/POST outcomes block further sweep and canonical
  writes until the intended labels are observed; only then may the sweep prepare its comment.
  The private journal is now `af.issue-triage-runner/3`; versions 1 and 2 are refused without
  rewriting or migration, following
  [ADR-0145](../docs/adr/0145-read-only-what-this-release-writes-and-version-every-changed-shape.md).
