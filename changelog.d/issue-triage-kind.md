- The issue-triage Task, staged for the owner to install
  ([`docs/design/issue-triage.md`](../docs/design/issue-triage.md), rollout step 2):
  `fixtures/issue-triage/` holds the `triage` kind on the report profile, the `backlog/triage`
  pipeline in the `kernel/report` shape, the `backlog/triage-author` (Codex `gpt-6-astra`,
  `read-source` only) and `backlog/triage-verifier` (Claude Opus 5.5) Workers with their shared
  catalog, contract fixtures and install steps, and an example Task for #432 that plans with
  zero Attempts. `schemas/issue-triage-v1.json` and `schemas/issue-triage-merged-v1.json` define
  `af/issue-triage@1` and `af/issue-triage-merged@1`; `scripts/backlog-check-triage.py`
  validates both triagers' documents, refuses a size below `L` for a named contract,
  schema-version, budget or sandbox-boundary change, an ADR with size `S` and a fixed or partial
  claim without a reachable commit or merged pull request, holds any `security` triage, and
  merges the two into one record (exit 0), a refusal (exit 1) or a hold (exit 3).
