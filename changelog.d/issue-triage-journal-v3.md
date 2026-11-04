- Version the private triage journal as `af.issue-triage-runner/3` for aborted auxiliary label
  checkpoints, with matching reader, writer and schema. The retained v2 schema rejects `aborted`;
  the current runner refuses v1 and v2 journals without migration or rewriting, following
  [ADR-0145](../docs/adr/0145-read-only-what-this-release-writes-and-version-every-changed-shape.md).
