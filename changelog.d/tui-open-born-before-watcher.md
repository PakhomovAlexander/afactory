- The `tui-open` performance driver records the browser's start before it watches the
  browser's exit, so a refused exit watcher no longer leaves the Linux cleanup unconfirmed (exit
  3) over an older process of the user whose environment cannot be read (#477); the driver's
  readiness helper in its tests reads the FIFO once more before it fails on an exited driver.
