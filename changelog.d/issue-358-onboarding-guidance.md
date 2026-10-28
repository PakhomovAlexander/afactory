- Make the first-run guidance truthful (issue #358). The installer's next steps and `af --help`
  recommend `af provider detect --apply`, as the README does, and its fish, bash and zsh PATH
  lines quote the bin directory as `af doctor` does, so a space, `$`, backtick, quote or `\` in it
  stays one literal path when the line is pasted; a bin directory with a `:`, which no PATH entry
  can hold, or with a control character, which the installer will not print legibly, gets no line
  to paste but one that says to set `XDG_BIN_HOME` to another directory and install again, a
  control character never printed. Beside a registered
  Provider that works, `af doctor` lists unregistered contexts as optional (`logged in` or `logged out` only
  where its status probe already asked, else `not probed`) instead of a warning whose
  `detect --apply` fix cannot clear it, and probes nothing more; for a `not probed` one it
  suggests `af provider detect` as an inspection that asks a CLI only about a safe directory
  holding a login marker, `--all` also about a safe empty one, never about an unsafe or missing
  one
  ([ADR-0206](../docs/adr/0206-detect-provider-logins-and-register-them-through-add.md)); a
  registered Provider whose CLI is missing or incompatible is no longer told to log in.
  `af review campaigns --json` prints the `--format json` document, and the two together are a
  usage error. The README and `af help json` claim `--json` only for commands whose `--help` lists
  it, promise one document only where one is printed, and name `af telemetry tail --json` as a
  JSON Lines stream, one `af/telemetry-tail@1` object per line, that a tail failing with an error
  ends with one `af/error@2` line, while a run two Stores record exits 2 with only stderr to say
  so. The README's install step says which PATH guidance each shell gets, and that a bin
  directory with a `:` or a control character gets none but the advice to install elsewhere. The
  README, with `af review group --help`, explains that looking alike, at one path or line or with
  similar titles, never makes Reports one Finding: they stay separate until an operator groups
  them, unless an explicit corroboration or an exact `rule_id` and `occurrence_key` attaches a
  Report to a Finding
  ([ADR-0006](../docs/adr/0006-finding-identity-is-path-independent.md)).
