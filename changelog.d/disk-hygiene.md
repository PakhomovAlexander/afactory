- Sandbox directories are named `af-sandbox-<pid>-…` under `$TMPDIR`, and every command that
  runs review or Task work (`af review run`, `af task start --execute`, `af task run`,
  `af provider doctor`) begins by removing the ones whose process no longer exists, finishing the
  removal before it exits. A sandbox is removed by the handle that owns it, so only a killed or
  aborted process leaves one behind, and a leftover tree that a Gate built into carries a whole
  `target/`: one development machine held 15 GB of them. The sweep keeps every directory whose
  process is still running and every directory the kernel preserved on purpose after an
  unconfirmed container cleanup (now marked `preserved` beside the tree), removes a tree only
  through directory descriptors opened without following links, touches nothing else under
  `$TMPDIR`, and reports what it removed in one stderr line.
