- Reduce repeated Task wait work with bounded, byte-verified artifact reuse, streaming canonical
  identity checks, indexed Gate history and unchanged Task observations. Artifact corruption and
  lease changes remain checked; status and timeout semantics are unchanged. Extend the full CLI
  benchmark across artifact, event and historical Task sizes, reporting total CPU, wall and RSS.
  See the [decision](../docs/adr/2026-11-10-reuse-validated-artifacts-for-task-observation.md).
