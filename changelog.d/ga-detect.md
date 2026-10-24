- `af provider detect` finds the `claude` and `codex` CLIs (on `PATH`, then in the usual install
  directories) and every auth directory on the machine — the CLI variables, `~/.claude` and
  `~/.codex`, their sibling names, the registry and `--dir` — asks each logged-in-looking
  directory's CLI what `af provider status` asks, and prints the one command that registers it;
  `--apply` registers every logged-in directory through `af provider add`'s publication, `--json`
  prints `af/provider-detect@1`, and with nothing found it prints the install commands. A directory
  without a login marker is never handed to its CLI, in `detect` or `status`, so neither creates
  `~/.codex` any more. `af provider add` refuses a directory that is not logged in (exit 6) unless
  `--unverified`; `af provider setup` creates a missing `--auth-dir` only with `--login`; its lock
  moved to `$XDG_STATE_HOME/af/locks`; each status row names one `setup … --login` fix
  (`--verbose` adds the CLI's own login command); `af help providers` explains the order of first
  use; and the Codex private-host login accepts the device prompt of codex-cli releases after
  0.159.2 (#305) ([ADR-0200](docs/adr/0200-detect-provider-logins-and-register-them-through-add.md)).
