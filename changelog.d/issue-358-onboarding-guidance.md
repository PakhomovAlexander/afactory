- Make the first-run guidance truthful (issue #358). The installer's next steps and `af --help`
  recommend `af provider detect --apply`, as the README does, and its fish PATH line quotes the
  bin directory as `af doctor` does, so one with a space stays one path. Beside a registered
  Provider that works, `af doctor` lists unregistered contexts as optional (`logged in` or `logged out` only
  where its status probe already asked, else `not probed`) instead of a warning whose
  `detect --apply` fix cannot clear it, and probes nothing more
  ([ADR-0206](../docs/adr/0206-detect-provider-logins-and-register-them-through-add.md)); a
  registered Provider whose CLI is missing or incompatible is no longer told to log in.
  `af review campaigns --json` prints the `--format json` document, and the two together are a
  usage error. The README and `af help json` claim `--json` only for commands whose `--help` lists
  it, promise one document only where one is printed, and name `af telemetry tail --json` as a
  JSON Lines stream, one `af/telemetry-tail@1` object per line, that a failing tail ends with one
  `af/error@1` line; the README, with `af review group --help`, explains that Findings that look
  alike stay apart until an operator groups them
  ([ADR-0006](../docs/adr/0006-finding-identity-is-path-independent.md)).
