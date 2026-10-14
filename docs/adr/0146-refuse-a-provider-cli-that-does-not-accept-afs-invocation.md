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
   same check then proves the app-server protocol. It starts the bare `codex app-server` in the
   same probe environment, sends `initialize`, accepts its answer and stops the process. It sends
   no account or rate-limit request, and the answer never counts as a login answer. The probe
   timeout and the Attempt deadline bound it. Only a JSON-RPC response with id 1 whose `result` is
   a JSON object proves the protocol. An app-server that exits before such a response, answers
   `initialize` with an error, answers it without a `result` object (`{"id":1}`, a `null` or
   non-object `result`) or writes, before that answer, a malformed line is refused as missing
   that protocol capability when the CLI's own `--version` succeeds. One line grammar judges every
   stdout line a probe reads, from the first byte until the response it waits for: a line is
   well-formed only as a JSON object that is a response (an `id` and exactly one of `result` or
   `error`) or a notification or request (a string `method`); anything else is malformed: not
   JSON, not an object (`[1,2]`), an `id` with neither `result` nor `error`
   (`{"id":2,"foo":"bar"}`), neither `id` nor `method` (`{"foo":"bar"}`). A malformed line
   refuses at once and af stops the app-server, never waiting on it as silence until the timeout;
   a notification, a request or a response to another id is skipped and the wait goes on, and a
   blank line is framing between messages. Every such refusal carries the process
   outcome, its exit status when it exited or the af-authored `was stopped by af` when it was
   still running after a one-second grace, and the bounded line it answered with, an error
   response included: `codex app-server exited 0 after answering initialize with an error:
   {"id":1,"error":{"code":-32600,"message":"bad init"}}`. The identity and usage probes read
   the `initialize` answer by the same rule, and every line after it until their id-2 answer by
   the same grammar, so a malformed line there makes admission and `status --usage` report the
   CLI `incompatible` at once, never a usage or identity timeout. A response answers a request
   only if af read it after writing that request: af remembers how much stdout it had read when
   it wrote the id-2 request and looks for the id-2 answer only in lines starting after that
   point. A well-formed id-2 response read earlier, before the `initialize` answer for example,
   answers nothing af asked and is skipped like a response to another id, so admission binds no
   principal and `status --usage` shows no limits from it; the wait goes on until a real answer,
   the app-server's exit or the timeout. af writes `initialize` before it reads any stdout. When `--version` also fails it is an ADR-0130 installation
   failure. A check af gave up on, by timeout, deadline or cancellation, leaves the CLI unchecked:
   `unavailable` in status, never `authenticated`. Plain status, `status --usage`, setup and
   admission therefore all give the same report for an app-server that cannot initialize. A help
   page that cannot be read follows the same rule: an installation failure when `--version` also
   fails, otherwise an incompatible CLI.
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
5. **Worker startup failures carry their own words.** A parsable result is, for Claude, a JSON
   object carrying the fields the adapter reads, a boolean `is_error` and the `result` (or, for a
   schema-constrained Attempt, the `structured_output`); `{}` and any other JSON are none. For
   Codex it is the `-o` final message of a process that exited 0 without an `error` or
   `turn.failed` event; a missing final-message file at exit 0, stdout holding only unrelated
   events and a non-zero exit after `turn.failed` are none. A Claude or Codex Worker that exits
   without a parsable result now has its exit status (`exited 1`, never a debug-printed `ExitStatus`) and
   a bounded, sanitized stderr excerpt in its Attempt diagnostic: its first error line and its last
   six lines, at most 1,024 characters, control characters and terminal escapes removed, and grant
   values redacted as before. This holds for every classification: `network`, every `auth_*`,
   `quota`, `capacity`, `model_unavailable`, `unknown` and `unsupported_invocation`. A recognized
   failure keeps its closed diagnostic first, as in `Codex Worker failed: Provider network request
   failed (network); it exited 1 without a parsable result; stderr: …`. The excerpt is read before
   the privacy filter replaces the captured stream, so an auth failure's diagnostic keeps the
   Worker's words while its Task evidence stays replaced; when either stream carries challenge
   material, which makes that filter replace both, the excerpt is the withheld marker alone. A failure whose output is the CLI's own argument error (`unknown option`, `unexpected
   argument`, `unrecognized …`, a `Usage:` block) is its own `NativeFailureKind`,
   `UnsupportedInvocation` (`unsupported_invocation`), whose diagnostic says the installed CLI
   does not accept af's invocation and names the upgrade. When the Worker left no parsable result,
   each adapter decides the classification at one place
   (`review_runner::cli::classify_without_result`), in this order: that argument error on stderr
   first, whatever the exit status (`0` included), the events a Codex Worker framed first, the
   native classification (an authentication hint on the same stderr among them) or the transport
   outcome; then the native classification; then `unknown`. A parsable result is never
   reclassified by what stderr says. Auth text beside an argument error still keeps the captured
   streams replaced. It is not an authentication failure, and
   its unknown-usage cause is `unreported`
   ([ADR-0143](0143-charge-zero-and-record-unknown-usage-when-no-usage-is-reported.md)).
6. **One credential filter for every Provider-authored line.** Every line of Provider text that
   may reach a diagnostic, a probe error or human output goes through
   `review_runner::cli::bounded_cli_line`. That covers stderr excerpts, app-server error lines,
   help and version lines, and the Codex rate-limit names and ids `af provider status --usage`
   shows in its table, where a withheld label keeps its window figures. Plan types and
   subscription labels are mapped to a fixed af vocabulary and never quoted. The function replaces the whole line with the af-authored
   `[line withheld: looks like credential material]` when `looks_like_credential` finds any of:
   - a key, token, secret, password, credential or auth assignment (`NAME=value`, `NAME: value`,
     `NAME="value"`, any case, any prefix such as `API_KEY`, `OPENAI_API_KEY`,
     `ANTHROPIC_API_KEY`);
   - `Bearer` and a token;
   - an `Authorization` header;
   - a token prefix (`sk-`, `sk-ant-`, `sk-proj-`, `ghp_`, `xoxb-` and their kin);
   - a run of 32 or more hexadecimal characters;
   - a run of 32 or more characters of the base64 and base64url alphabet (`A-Z`, `a-z`, `0-9`,
     `+`, `/`, `=`, `_`, `-`) holding at least one upper-case letter, one lower-case letter and
     one digit;
   - a JSON Web Token: exactly three non-empty base64url runs (`A-Z`, `a-z`, `0-9`, `_`, `-`)
     joined by single dots, the first starting with `eyJ` and 16 or more characters long, the
     third 16 or more characters long, bounded by a character outside that alphabet or the
     line's ends, so `error: eyJabc.def. ordinary failure` passes;
   - login-challenge material.

   A run of letters alone is a word, a long option name or a path segment, not a secret, and
   passes by design, so executable paths stay readable. The line is judged whole, before it is
   cut to its bound. A stream carrying challenge material anywhere is still withheld as a whole,
   now as that marker rather than as silence. The exit status and the classification always stay; only the line goes.
7. **No weaker invocation, ever.** No isolation flag is dropped, reordered behind package flags or
   replaced to make an older CLI run, and there is no fallback invocation for an older CLI. The
   capability check never runs paid inference, never alters credentials, and its diagnostics carry
   no secret; Provider-authored text reaches stderr, the human table and Attempt diagnostics under
   ADR-0130's bounds and the filter above, and never a versioned document or retry feedback.

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
- **Prove the Codex protocol only where a probe already starts the app-server.** Rejected. Plain
  status never starts it, so it called a Codex whose app-server exits at once `authenticated`,
  setup registered it, and `status --usage` blamed only the usage axis. Only admission refused it.
  The `initialize` round trip is token-free and costs one bounded process, so every surface that
  names a CLI usable runs it.
- **Quote the excerpt only for unclassified and `unsupported_invocation` failures.** Rejected. A
  recognized category says what kind of failure happened, but not which proxy, file or runtime
  failed; the Worker's own line says that, and without it an operator cannot act.
- **Drop the whole stream when any line looks like a secret.** Rejected for the line-level
  rules. Withholding only the offending line keeps the CLI's other words, such as the argument
  error before a leaked key. Challenge material, which a terminal may wrap across lines, still
  withholds the stream.
- **Withhold every run of 32 or more base64 characters.** Rejected. Letters alone make such a
  run as readily as a key does: a long option, a path such as
  `/Users/alice/bs/apkhmv-main/af/afactory-wt-cli-compat/crates/af`, and the report would lose
  the executable it names. A key or token that long mixes cases and digits, or is hex, or is a
  JWT.
- **Recognize only the spellings seen so far.** Rejected. `API_KEY=` reached a diagnostic because
  the filter knew `token=` and `access_token` but not that spelling. The filter now matches names
  by their secret-bearing part and token shapes by their form.
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
  more, `--version`. A Codex row also starts its app-server once for `initialize`, so a Codex
  whose app-server cannot initialize is `incompatible` on plain status, and a slow one is
  `unavailable` rather than `authenticated`. A CLI whose help page is unreadable is no longer
  usable even if its login probe answers. Test doubles must answer the help page, and Codex
  doubles a bare `app-server` `initialize`; the fixture CLIs in this repository do.
- codex-cli 0.121.0 works again with every af probe.
- `provider-status-v2.json` replaces `provider-status-v1.json`. Consumers of `--json` read
  `af/provider-status@2`; the only change is the `incompatible` state and the exit-4 rule that
  covers it.
- Worker diagnostics that used to print `Ok(ExitStatus(unix_wait_status(256)))` now print the exit
  status and the CLI's own last words, bounded, for every classification.
- A diagnostic, probe error or table note may now show `[line withheld: looks like credential
  material]`, including for a long hex or base64 run that was not a secret. af errs toward
  withholding.
