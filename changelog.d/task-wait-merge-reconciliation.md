- Reconcile Task wait validation reuse, indexed Gate history and full CLI benchmarks with the
  current crate and module layout. Preserve byte-level integrity checks, lease handoffs and total
  CPU, wall and RSS coverage. Isolate doctor regression tests from ambient Provider CLIs and
  check the precise setup hint both without Provider binaries and with a logged-out Codex CLI.
- Recheck compiled telemetry benchmark binaries by their bytes after copying, so a same-size
  mutation with unchanged metadata cannot pass binary admission.
