# ADR-0146: Refuse a Provider CLI that does not accept af's invocation

**Status:** accepted (2026-10-14). Extends
[ADR-0130](0130-report-a-provider-cli-that-cannot-start.md): exit code 4 also covers a CLI that
starts but does not accept af's invocation, and `af provider status` gains the `incompatible` state
in a new document version, `af/provider-status@2`
([ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)).

## Context

On a machine with Claude Code 2.1.101 and codex-cli 0.121.0, `af provider status` reported both
Providers authenticated
([issue #259](https://github.com/PakhomovAlexander/afactory/issues/259)). Then:

- The Claude canary failed at Task admission with `Claude Worker failed with
  Ok(ExitStatus(unix_wait_status(256)))`. The cause, visible only in the Worker's stderr, was
  `error: unknown option '--safe-mode'`. Claude Code added `--safe-mode` in 2.1.169 and
  `--restricted` in 2.1.248; 2.1.295 fixed `--tools` and `--restricted` not applying to built-in
  tools that register after launch.
- The Codex canary failed at planning with `No Pipeline selected: {"kind":"unavailable"}`, reason
  `Codex app-server probe exited without a rate-limit response`. The cause was clap's `error:
  unexpected argument '--stdio' found` on the stderr the probe discarded: codex-cli 0.121.0's
  `app-server` has no `--stdio`, and serves stdio by default. 0.162.1 accepts `--stdio`, `--listen
  stdio://` and the bare default alike.

ADR-0130 asks whether a CLI starts. Both CLIs started, answered their login probes, and still could
not serve af. Authentication success said nothing about whether the installed CLI accepts the
invocation af builds, and the diagnostics hid the one line that said so.

## Decision

1. **The Codex app-server is started bare.** Every af probe runs `codex app-server` with no
   transport flag, the documented default `stdio://` on every verified version, and keeps the
   `initialize`, `initialized`, request protocol. Its stderr is kept, bounded, with its exit
   status: an app-server that exits before answering `initialize` is reported as `codex app-server
   exited 2 before answering initialize: error: unexpected argument '--stdio' found`, never as a
   missing response.
2. **Each adapter owns the list of options its invocation passes.** `CLAUDE_INVOCATION` and
   `CODEX_INVOCATION` sit next to the command builders in `review-runner-claude` and
   `review-runner-codex`, each option in the spelling the builder emits. A unit test in each crate
   holds the list equal to every option any Attempt's command can emit, so the list cannot drift
   from the invocation. The isolation flags (`--safe-mode`, `--restricted`, the tool grants, the
   Codex sandbox mode) are on it.
3. **One token-free capability check, before anything asks about a login.** `af provider status`,
   `af provider setup` and Task Provider admission (`TaskProviderIdentity::probe`) each run the
   same check on the resolved executable first. It runs the (sub)command's help page, `claude
   --help` or `codex exec --help`, in the probe environment (an ADR-0144 probe directory, the
   sanitized `PATH`, no credential read or written, no model call), reads the options the page
   declares, and refuses the CLI unless every listed option is among them. The decision is made on
   the help page, never on a version string, so a wrapper or fork is judged by what it accepts.
   `--version` runs only once a CLI is refused, to report the version it prints. For Codex, the
   app-server protocol is proven by the identity probe's own `initialize` round trip with the bare
   invocation. An app-server that exits before `initialize`, on a CLI whose own `--version`
   succeeds, is refused as missing that protocol capability; one whose `--version` fails is an
   ADR-0130 installation failure. A help page that cannot be read follows the same rule: an
   installation failure when `--version` also fails, otherwise an incompatible CLI.
4. **An incompatible CLI is reported as itself.** The report names the Provider ID and kind, the
   executable path, the detected version, every missing option or protocol capability, and the
   remediation, the Provider's own upgrade command (`npm install -g
   @anthropic-ai/claude-code@latest`, `npm install -g @openai/codex@latest`):
   - `af provider status` gives the context the state `incompatible`, usability `unusable`,
     whatever its login, and exits 4, the code ADR-0130 gave to a CLI that cannot start. Exit 4 now
     means "the CLI as installed cannot serve af". The document is `af/provider-status@2`
     (`provider-status-v2.json`); `@1` is neither written nor read.
   - `af provider setup` reports `provider_cli_missing`, exit 4, before any login is asked about or
     started, with the af-authored summary as its diagnostic. The setup document's shape is
     unchanged.
   - Task admission refuses with `Task Provider admission refused before any Worker was
     dispatched: …` carrying the report, before any Worker process starts and before any Attempt is
     reserved or charged. A refused selection's stderr line now also names each candidate's own
     reason after `No Pipeline selected: …`. None of these is worded as a login or account problem.
5. **Worker startup failures carry their own words.** A Claude or Codex Worker that exits without
   a parsable result now has its exit status (`exited 1`, never a debug-printed `ExitStatus`) and
   a bounded, sanitized stderr excerpt in its Attempt diagnostic: its first error line and its last
   six lines, at most 1,024 characters, control characters and terminal escapes removed, grant
   values redacted as before, and nothing at all when the stream carries credential or challenge
   material. A failure whose output is the CLI's own argument error (`unknown option`, `unexpected
   argument`, `unrecognized …`, a `Usage:` block) is its own `NativeFailureKind`,
   `UnsupportedInvocation` (`unsupported_invocation`), whose diagnostic says the installed CLI
   does not accept af's invocation and names the upgrade. It is not an authentication failure, and
   its unknown-usage cause is `unreported`
   ([ADR-0143](0143-charge-zero-and-record-unknown-usage-when-no-usage-is-reported.md)).
6. **No weaker invocation, ever.** No isolation flag is dropped, reordered behind package flags or
   replaced to make an older CLI run, and there is no fallback invocation for an older CLI. The
   capability check never runs paid inference, never alters credentials, and its diagnostics carry
   no secret; Provider-authored text reaches stderr, the human table and Attempt diagnostics under
   ADR-0130's bounds, and never a versioned document or retry feedback.

## Considered options

- **Decide on the version string.** Rejected. Wrappers, forks and vendor builds print versions af
  cannot interpret, and a version floor would have to be maintained by hand against two
  changelogs. The help page states what the executable actually accepts. The version is reported,
  not judged.
- **Fall back to an invocation without `--safe-mode` or `--restricted` on an older Claude Code.**
  Rejected. Those flags are the Worker's isolation boundary
  ([ADR-0042](0042-require-provider-bindings-and-isolate-claude-reviewers.md)). Running without
  them would widen Worker authority to make an old CLI work.
- **Keep `--stdio`, or switch to `--listen stdio://`.** Rejected. The bare invocation is the
  default on every verified version and works on both; a transport flag only adds a way to break.
- **Probe only at Task admission.** Rejected. Status called the CLI authenticated, and an
  operator reads status first. Every surface that names a CLI usable runs the same check.
- **Run the check only after an authentication answer, and only for authenticated contexts.**
  Rejected. A logged-out context with an incompatible CLI would send the operator to log in first,
  and the login would not help. The check runs first and the incompatible CLI is reported as such.
- **A separate exit code for `incompatible`.** Rejected. Exit 4 already means "the CLI as
  installed cannot serve af" to an operator, and one code per condition keeps the ADR-0112 table
  small.
- **A new setup result and `af/provider-setup@2`.** Rejected for now. Setup's closed
  `provider_cli_missing` result and exit 4 already say the CLI cannot serve af. The af-authored
  diagnostic says why, so no new shape is needed to act on it.
- **Re-run the capability check before every private Worker send.** Rejected. ADR-0130 rejected
  re-diagnosis after every invocation for its cost and false positives, and the check holds for the
  executable admission resolved. A client that updates itself mid-Task (ADR-0126) to an
  incompatible version is caught by the Worker's `unsupported_invocation` classification, and a
  Codex app-server that stops answering `initialize` is caught by the identity recheck before the
  send.

## Consequences

- An operator whose CLI is too old sees the Provider, the path, the version, every missing option
  and the upgrade from `af provider status`, setup or the Task command that refused. No Worker
  runs and no token is charged before they do.
- Every status row and every admission runs one more process, the help page, and a refused CLI one
  more, `--version`. A CLI whose help page is unreadable is no longer usable even if its login
  probe answers. Test doubles must answer the help page; the fixture CLIs in this repository do.
- codex-cli 0.121.0 works again with every af probe.
- `provider-status-v2.json` replaces `provider-status-v1.json`. Consumers of `--json` read
  `af/provider-status@2`; the only change is the `incompatible` state and the exit-4 rule that
  covers it.
- Worker diagnostics that used to print `Ok(ExitStatus(unix_wait_status(256)))` now print the exit
  status and the CLI's own last words, bounded.
