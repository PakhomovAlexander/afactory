- Help that runs and refusals that name the fix. Every example in every `--help` page and
  `af help <topic>` now runs as written in an onboarded repository, or says what it needs first
  with `# needs: …`; a test runs them all. `af review plan` refuses, at exit 1, a
  `--provider NODE=ID` whose Provider this machine has not registered or whose node is no model
  Worker of the pipeline, in the words `af review run` uses, and ends with a `next` line, the
  exact `af review run` command once the plan is ready. A repository without `.af/`, an unknown
  `--policy-rev`, a missing binding and an empty diff each say what to do next. Things af cannot
  find are said in one shape, `<Thing> <name> not found in <where>; <fix>`, and a malformed
  Task file names the field path and the shape it takes. `docs/README.md` gains a
  troubleshooting table of the messages people meet most.
- A Store names the af release that wrote it. Every Store records each release that opens it
  for writing (`af/store-writer@1`, beside the event log), so a release that cannot replay
  another release's Store, which it still refuses (ADR-0145), says `Store … was written by af
  <v> (this is af <w>); fix: AF_VERSION=<v> af …`; `af task list`, `af review campaigns`, the
  browser and `af storage` list such Stores in one row by release. A Store from before this
  release is named as an older release's.
- The canonical Document text escapes only what is Markdown markup where it stands, so paths and
  ordinary punctuation read as written; its payload is now `af.document/2`, and a Document of
  `af.document/1` is not read. `af task output --format markdown` writes the readable
  presentation of a Document or report; `--format canonical-markdown` writes the recorded bytes.
- `af provider list` and `af providers …` work, `af review tui` points at the browser, `af
  provider doctor` shows only its own flags and requires `--campaign`, `af self --help` leads
  with `status` and `update`, `af review --help` groups its commands by a Campaign's life, and
  `af onboard --help` reads as four steps for a person. The roots af keeps state under are
  resolved once, so a home under a symlink (macOS `/tmp`) no longer prints `skipped … is a
  symlink`, and a path `af storage` cannot measure is a note of the new `af storage --verbose`
  rather than a warning after every run. Human text shows sizes, durations and local times in
  units; `--json` keeps the raw figures.
