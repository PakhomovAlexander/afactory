- Remote Checks: a Task whose selected root Pipeline is pinned in the catalog and tagged exactly
  `ci` may send a candidate that changes `.github/` to its Remote Check. Every record of that
  phase names the granting run authority, plan and Pipeline as `trusted_ci`, and every reader
  recomputes it. Pipelines gain an optional `tags` set, absent by default. Any other candidate
  that changes `.github/` is still refused
  ([ADR-0141](docs/adr/0141-let-a-pinned-ci-tagged-root-pipeline-send-a-changed-workflow.md)).
