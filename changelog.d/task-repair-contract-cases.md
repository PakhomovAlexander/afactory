- Tests: the real-process `task_repair` case for a fix verifier that names a stale Finding view is
  replaced by the `stale_snapshot_view` contract case, which applies the same mutation to the
  shared `validate_context` predicate behind `validate_fix_output` and `repair_acceptance`; exit 4,
  8 Attempts and an unchanged replay stay asserted by the stale-subject case. The claim-less receipt
  case stays real-process, because the Worker output schema rejects it before `validate_context`.
