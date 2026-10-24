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
