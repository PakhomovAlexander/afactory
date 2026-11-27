- Check the exact predecessor Round, Findings input and reviewed candidate when a Scenario
  reopens a recorded repair plan.
- Refuse an explicit Codex session continuation request before starting a Task when
  AF has no verified App Server transport for inherited history. See the
  [ADR](../docs/adr/2026-11-27-bind-codex-continuation-to-the-first-reviewed-repair.md).
- Add Store contracts for Codex dispatch fencing and sealed producer checkpoints. Execution
  records are now `af/TaskExecutionRecord@6`, exposed by `af/task-inspection@23`.
- Verify the producing Attempt's admitted source and the sealed SourceTree's provenance before
  selecting a Codex checkpoint. Re-pin the guarded performance harness to this source.
