- Structured Task Workers now make one model call per declared Attempt. Invalid replies and
  transient failures settle that Attempt; further calls require the scheduler's explicit retry
  allowance. Exact schema and provenance checks remain enforced. See
  [the decision](../docs/adr/2026-11-05-limit-structured-worker-attempts-to-one-model-call.md).
