- A Task-file refusal that ends naming the command to run next (`acceptance` on a review,
  document, report or optimization Task, or a Pipeline without the `acceptance` input, and
  `limits.wall_ms=0`) no longer prints that command inside one over-wide line on a terminal: the
  sentence is wrapped to the terminal and ends `then run this command again:`, and the command
  follows on a `next:` line of its own, continued between its arguments, so it runs as pasted.
  Piped stderr and the `--json` document are unchanged. The performance laboratory and its tests
  run again on Python 3.9 (macOS's `/usr/bin/python3`), as ADR-0162 requires: the container
  init regression no longer needs `os.waitid`, and the supervisor's and telemetry run's added
  error notes no longer need `BaseException.add_note`.
