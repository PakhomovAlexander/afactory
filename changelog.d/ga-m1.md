- A Task that ended at a failed gate is `finished/gate_failed/CAUSE` and exits 5 wherever it is
  shown, gate retry included, and its one `next:` line is `af task run ID --retry-gate` only when
  the kernel would run that gate again on the same candidate; otherwise it is `af task show ID
  --checks` (ADR-0196). `af task show ID --json` carries the gate's check reports
  (`gate.check_reports`, each check node's latest `af/TaskCheckReport@1`) and its retries
  (`gate.retries`, every `TaskGateRetried@1`) in `af/task-inspection@12`; `af task report` counts
  each `af task run ID --retry-gate` as a run of the Task and an `af task refresh` as none. The
  run log's last line on stderr, the Task's state and next command, now comes after what the
  stale sandbox sweep and the Storage Budget sweep that end a run print.
