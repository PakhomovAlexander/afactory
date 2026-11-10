- Release pin PRs now require a matching credential-free GitHub HTTPS origin and use the scoped
  PAT for Git operations. SSH origins, embedded credentials, URL rewrites and scoped credential
  overrides fail before publication, enforcing the
  [release decision](../docs/adr/2026-11-09-review-release-contracts-and-pins-before-merge.md).
- Retain the current focused performance contract test results and the supplied Linux contained
  telemetry smoke evidence, bound to the repair-1 Snapshot and binary. The 4 GiB failure and
  successful current-capacity paths are smoke evidence; hosted rollout remains unproven.
