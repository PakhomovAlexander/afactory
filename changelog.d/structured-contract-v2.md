- Structured packages now require `af.structured-contract/2`, identifying one model call per
  Worker Attempt. Upgrade the AF client, review and repin consuming packages; v1 contracts are
  refused without fallback. Older clients reject v2 during digest/planning before model calls.
  See [the decision](../docs/adr/2026-11-05-version-the-single-call-structured-contract.md).
