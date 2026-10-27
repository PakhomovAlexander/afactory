- An interrupted command under `--json` prints `af/error@2` (version 1 is no longer printed):
  `cause: interrupted`, exit 130 or 143, `details` with the `signal` and the `task`, and
  `next_action` the command that continues it, `af task run TASK_ID` with the selectors given,
  so a script no longer parses the sentence
  ([ADR-0256](../docs/adr/0256-carry-the-recovery-of-an-interrupted-command-as-a-typed-field.md)).
  Every `af/error@2` document's `next_action` is null where no pasted line can carry it, a
  selector with a tab or a newline, or a line past 4096 characters, so each validates against
  `schemas/error-v2.json`; a `recovery_exhausted` document, whose fix the schema requires, then
  carries `af task start --file TASK_FILE` without the selectors, which its sentence completes.
- On a terminal the `next:` lines of `af task show`, `af task run` and `af task run --detach`,
  the run log's last line, and the log a detached run that ended at once replays, are held to
  its width; without one they stay whole for the script that reads them. In those lines, and in
  `af task list`, `af review plan` and `af doctor`, no line is wider than the terminal: an
  argument wider than a line, a long `--state` path or the program's own path, is cut into
  quoted pieces a shell joins, a label too wide for the command beside it (`Task ID: STATE;
  next:`) is wrapped first, and each runs as printed.
- `af doctor` offers a fix only where running it can clear its check: no `af self update --check`
  for an `AF_RELEASE_SOURCE` that is no directory, no `af onboard --repo` preview (the hint gives
  the preview and the `--apply`, the sentence's `;` outside the path), and no `af provider detect
  --apply` unless a CLI on PATH reports an unregistered login of the kind needed logged in (a
  marker alone, a logged-out context or an empty `~/.claude` registers nothing; the hint gives
  the setup to run at a private terminal). A project whose Worker needs a kind no login is found
  for names that kind (`no codex login was found to register`) and never denies a login of
  another kind that `af provider detect --apply` would register. With the default link absent, the PATH fix
  installs first, also when its directory is already on PATH, and an install with no `af` on
  PATH runs this binary by its path; a long PATH line, the elvish one included, is cut where its
  shell joins it again. A broken `.af/` is named where `af onboard` stops, by taking its steps in
  its order: a Worker package's pin before its typed `reviewer.toml`, a typed error with its file
  and line, a changed package with the `--refresh-lock` that re-pins it, a Worker package too
  large or unreadable named at that package rather than at the pipeline, and only paths below
  `.af/`. An install fix that runs this binary by a long path, alone or chained by `&&` to the
  PATH line, is broken where its shell joins it, its path in fish's quoting under fish, and
  pastes and runs as printed at 80 and 100 columns; its quoting is read as that of the shell it
  is printed for, so a path holding a backslash is never cut inside fish's `\\` escape.
