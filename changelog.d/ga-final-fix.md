- `af provider detect` recognizes the default Claude context `af provider status` accepts, a
  `~/.claude` with no marker inside it and a regular `~/.claude.json` beside it: both commands ask
  one predicate, so detect lists it (marker `~/.claude.json`), probes it and `--apply` registers
  it as `claude-main`, and status probes the registered Provider instead of calling it
  `not_probed`. `af provider detect --json` prints `af/provider-detect@3`
  (`schemas/provider-detect-v3.json`; versions 1 and 2 are neither printed nor read), whose next
  actions are typed fields: `install`, one `{kind, command, docs}` per kind whose CLI was not
  found, and `next_action`, the scan to run again with its own `--kind`, `--dir`, `--id` and
  `--all` when no CLI was found
  ([ADR-0197](docs/adr/0197-detect-provider-logins-and-register-them-through-add.md),
  amended).
- `af task findings ID --round N` shows round N's own evaluator verdict, the one the Task log
  recorded after that round, instead of the last one the current execution holds, and a
  continuation (`af task run NEW --from TASK --round N`) hands its implementer that verdict, not
  the verdict of another round that reviewed the same Snapshot.
- `af task report --pr-body` refuses a lines added block that `scripts/pr-lines.py` printed
  incomplete, without its end marker after the begin marker or its `**Lines added: …**` title,
  with exit 1 and a message naming what is missing.
- `af help start` binds its review command to the Providers registered on this machine, as
  `af onboard` binds them, and prints `PROVIDER` with the reason when none is registered; the
  README shows the placeholder form and says that `af onboard` prints the exact command.
  `af help agents` teaches `af task run NEW_ID --from TASK_ID [--round N]` for another round,
  keeping the hand-made source binding only as the form that carries no findings.
- `af doctor`'s rows fit 100 columns, or the terminal's width when narrower (at least 60): a long
  detail or path continues on indented lines, broken after a `/` and never cut short, and each
  fix command stays whole on its line or is broken between its arguments.
- `af task report --pr-body` accepts only a lines added block that `scripts/pr-lines.py`'s own
  parser reads back: the title, then the table header, its separator and at least one row, each
  directory once, or `**Lines added: none**` alone. A title without its table is refused with
  exit 1, naming what follows the title, instead of printing a body `scripts/check-pr-report.py`
  refuses.
- A continuation from an earlier review round (`af task run NEW --from TASK --round N`) records
  the provenance of that round's own candidate: its source origin names the `af/TaskOutput@1`
  record that published the candidate, not TASK's result when the result holds a later Snapshot.
  A round whose candidate no recorded output holds is refused with `af/error@1`, and binding a
  source refuses any reference whose output holds a Snapshot other than the bound one
  ([ADR-0203](docs/adr/0203-continue-a-task-from-its-reviewed-candidate-and-give-judging-workers-the-base.md),
  amended).
- `af doctor`'s fixes are commands a shell runs as given, and what a command cannot say moves to a
  new hint: `af doctor --json` prints `af/doctor@2` (`schemas/doctor-v2.json`; version 1 is
  neither printed nor read), each check with `fix` and `hint`, and the text prints a `hint:` line
  under the fix. The shell check names the shell `$SHELL` names (`af self setup-shell --shell zsh
  --write`), or `--shell SHELL` with the choices in its hint; the project check's fix is `af
  onboard --repo PATH`, its hint says it is a token-free preview; the PATH fix is the `export` or
  `fish_add_path` line, its hint where it belongs. A problem no single command fixes (no known
  package manager, Spotlight indexing, an unreadable lock) has a null `fix` and a hint that says
  what to do ([ADR-0201](docs/adr/0201-make-the-first-run-say-what-to-do-next.md), amended).
- The pseudo-terminal test helpers pass the test process's `TMPDIR` (and, for `af provider
  detect`'s, the fixture's home and XDG directories) to the child they start after clearing its
  environment, so the tests pass in a sandbox whose `TMPDIR` is private.
