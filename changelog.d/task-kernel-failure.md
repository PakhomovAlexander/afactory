- Report failed native Task operations as `execution_failed/kernel_execution` (exit 8),
  retaining the diagnostic and a command to inspect it. Empty Diff Subjects remain refused;
  resource exhaustion keeps its existing outcome and exit code. Public state documents take
  new versions; the persisted TaskResult shape is unchanged.
