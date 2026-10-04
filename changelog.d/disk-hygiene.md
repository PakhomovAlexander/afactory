- Sandbox directories are named `af-sandbox-<pid>-…` under `$TMPDIR`, and every `af` command
  starts by removing the ones whose process no longer exists. A sandbox is removed by the handle
  that owns it, so only a killed or aborted process leaves one behind, and a leftover tree that a
  Gate built into carries a whole `target/`: one development machine held 15 GB of them. The sweep
  keeps every directory whose process is still running, touches nothing else under `$TMPDIR`, and
  reports what it removed in one stderr line.
