# Providers

How a machine gets the model access `af review` and `af task` need, and what that flow promises a
caller that is not a human sitting at a terminal.

A **Provider** is an operator-named, machine-local authentication context for one adapter kind
([`../CONTEXT.md`](../CONTEXT.md)). `af` never holds a credential: the official `claude` or `codex`
CLI owns it, and the machine-local registry at `~/.config/af/providers.toml` records only an ID, a
kind, and an auth directory. Bootstrap runs in the invoking release rather than a repository's
pinned one, and registry publication stays atomic for older readers
([ADR-0111](adr/0111-keep-provider-bootstrap-machine-local-and-cross-release-safe.md)).

## The default terminal boundary

An interactive Provider login prints an **OAuth URL and an authorization code**. Those are
credentials in transit. Anything that can read them — a pipe, a chat window, an agent transcript, a
CI log — is a place they must never reach through ordinary output. An explicitly permissioned
private host can instead use [the dedicated host protocol](provider-auth-host.md); its human-private
transport is separate from every ordinary command stream.

So `af provider setup` starts an official login only when **both** are true:

1. the operator passed `--login`, and
2. this process owns an interactive terminal on stdin, stdout **and** stderr.

Otherwise setup fails fast with the `human_action_required` result, exit code 3, and the exact
command a human should run at a private terminal. In this default path, `af` does not proxy, broker, or headlessly
complete an OAuth flow, and it never captures, parses, stores or re-emits what that login prints
([ADR-0112](adr/0112-refuse-agent-mediated-provider-logins.md)).

## Onboarding a Provider

Already authenticated with the official CLI? Then no login is needed and none is started:

```sh
af provider setup codex-main --kind codex            # registers an authenticated context
af provider setup codex-main --kind codex --json     # the same, as one JSON document
```

Not authenticated yet? Run this yourself, in your own terminal:

```sh
af provider setup codex-main --kind codex --login
```

`--login` prints the warning above, hands the terminal to the official CLI, verifies the result,
and only then writes the registry entry. It cannot be combined with `--json`: JSON is the
non-interactive automation surface, while login deliberately hands the private terminal to the
Provider CLI. Re-running any of these is safe. `af provider add` registers an already-authenticated
context and never offers a login at all.

Generic automation should run `setup` **without** `--login` and respect the terminal-only refusal.
An assistant with an authorized private host integration should own browser-only setup using
[`af provider auth`](provider-auth-host.md), rather than ask its user to obtain SSH or terminal
access. This explicit path also reauthenticates a stale context even if local status says logged in.
Without the private host capability, report that narrow integration blocker; never publish raw
OAuth output to a shared conversation. Authentication completion remains `authenticated_unverified`.

`provider auth begin/status/cancel` are machine-local, have a separate versioned non-secret status
schema, and never invoke model verification or resume Tasks. See the host contract for lifecycle,
expiry, cancellation, supported providers, and the remaining runtime scope of issue #122.

## Removing a Provider

```sh
af provider remove codex-main                # drop one registry entry
af provider remove claude-work codex-work    # drop several in one publication
```

`remove` deletes registry entries and nothing else. Each auth directory, and the login its
Provider CLI keeps there, is left untouched, so `af provider add` or `setup` registers the context
again. The registry is rewritten through the same locked, atomic publication `add` uses, and the
version it replaced is preserved; the command prints where
([ADR-0136](adr/0136-remove-a-registered-provider-by-id.md)). A comment directly above a removed
entry goes with it.

The command is all or nothing. An ID that is not registered refuses it with exit 1 and removes
nothing. `af` does not check whether a pipeline, Campaign or Task still binds a removed ID.

Two cases come up often:

- **A deleted auth directory.** One entry whose auth directory no longer exists makes the whole
  registry invalid: `status` warns and lists no registered Provider. Removing that entry is the
  repair. When several are stale, name them all in one command, because `af` publishes only a
  registry that is valid.
- **An ambient ID.** `claude-ambient` and `codex-ambient` are discovery labels, not registry
  entries, so there is nothing to remove. One is listed while the directory the Provider CLI uses
  by default (`CLAUDE_CONFIG_DIR` or `~/.claude`; `CODEX_HOME` or `~/.codex`) is not a registered
  Provider's auth directory. Once a Provider of its kind is registered, it is left out when its
  status is `not authenticated`; with a login, or when the status probe fails (`unavailable`,
  `installation failed`), it stays listed ([ADR-0141](adr/0141-list-a-logged-out-default-context-only-until-its-kind-is-registered.md)).
  Start `af` with that variable naming a registered directory, or register the default
  directory, and the label goes away.

In the browser, `d` on a registered Provider fills the `:` line with `provider remove ID`, and
Enter runs it.

## Checking a Provider

```sh
af provider status                 # fast: the registry, the CLI capability check and one authentication check per context
af provider status --json          # the same, as af/provider-status@2
af provider status --usage         # additionally probe subscription and quota windows
af provider doctor --campaign NAME --provider correctness=codex-main
```

`status` is deliberately cheap and says only what it checked. It reports four independent axes:

| Axis | Values | Means |
|---|---|---|
| `registered` | true / false | in the registry, so selectable by `--provider NODE=ID`; false is an ambient discovery label |
| `auth` | `authenticated`, `not_authenticated`, `unknown`, `unavailable`, `installation_failed`, `incompatible`, `not_probed` | what the Provider CLI said about this auth directory; `installation_failed` means the CLI cannot start, so it said nothing; `incompatible` means it starts but does not accept af's invocation, so its login was not asked about |
| `usability` | `usable_or_untested`, `unusable`, `unknown` | status never claims `usable`: only `af provider doctor` dispatches and can prove it |
| `usage.state` | `not_requested`, `not_applicable`, `unsupported`, `available`, `unavailable` | the optional plan and quota axis |

Usage is separate on purpose. A subscription probe can time out, be unsupported for an API-key
context, or be missing entirely — none of which says anything about whether the Provider is
authenticated. An unanswered probe leaves `auth` and `usability` exactly where they were and exits
7. `af provider doctor` remains the charged, explicit end-to-end check, and is never implicit.

## A Provider CLI that cannot start

An update can leave the official CLI unable to start at all. One npm auto-update left `codex`
without its platform package, and every invocation failed at once with the CLI's own error
([issue #136](https://github.com/PakhomovAlexander/afactory/issues/136)):

```text
Error: Missing optional dependency @openai/codex-darwin-arm64. Reinstall Codex: npm install -g @openai/codex@latest
```

`af` reports this as a **Provider installation failure**, never as a login, credential or model
problem ([ADR-0130](adr/0130-report-a-provider-cli-that-cannot-start.md)). A CLI cannot start when
any of these is true:

- the program is not on PATH;
- a file of that name is on PATH but is not executable;
- the operating system refuses to start it;
- it exits non-zero on its own `--version` check, before doing any provider work.

`af` runs the version check only when the CLI's own status or account probe got no answer it
recognizes. A CLI that answered, even to say "not logged in", has started. A probe that timed out
or was cancelled proves nothing, so neither triggers the check. The report names the Provider ID,
the program path, the CLI's own first error line and the fix it suggests, for example:

```text
af provider status: provider codex-main: Provider installation failure: the codex CLI `/opt/homebrew/bin/codex` exited 1 on its own `--version` check before doing any provider work: Error: Missing optional dependency @openai/codex-darwin-arm64. Reinstall Codex: npm install -g @openai/codex@latest; fix: Reinstall Codex: npm install -g @openai/codex@latest
```

| Where | What happens |
|---|---|
| `af provider status` | The context's `auth` is `installation_failed` and its usability `unusable`. The command exits 4, which outranks 7. stderr carries one line per broken Provider, and the table's note repeats it. |
| `af provider setup` | `provider_cli_missing`, exit 4, instead of `authentication_failed`. The document's `diagnostic` is the af-authored summary; the CLI's words go to stderr only. |
| Task Provider admission (`af task plan`, `af task run`, `af review run`, `af provider doctor`) | Refused before any Worker is dispatched and before any Attempt is reserved or charged. The error begins `Task Provider admission refused before any Worker was dispatched`, under the usual exit 1. |
| A Worker Attempt after admission | The identity recheck before the private send finds the CLI cannot start. The Attempt fails with zero charge and the diagnostic `Provider environment failure, not a model or credential failure: …`, never as a changed account or a failed model reply. |

The CLI's error line and suggested fix are Provider-authored text. They are bounded to one line
each, with terminal escapes and control characters removed. They appear on stderr, in the human
table and in Attempt diagnostics, and never in a versioned JSON document or in retry feedback.
Fix the installation with the suggested command, then rerun `af provider status`.

## Supported CLI versions and required capabilities

Authentication success never implies invocation compatibility. A machine with Claude Code 2.1.101
and codex-cli 0.121.0 once reported both Providers authenticated; the first Claude Worker
then failed on `error: unknown option '--safe-mode'`, and every Codex probe on `error: unexpected
argument '--stdio' found` ([issue #259](https://github.com/PakhomovAlexander/afactory/issues/259)).
So before `af provider status`, `af provider setup` or Task Provider admission asks a CLI about its
login, `af` runs a token-free **capability check** of the resolved executable
([ADR-0146](adr/0146-refuse-a-provider-cli-that-does-not-accept-afs-invocation.md)):

- It runs the (sub)command's help page — `claude --help`, `codex exec --help` — in the probe
  environment: a fresh probe directory, the sanitized `PATH`, no credential read or written, and
  no model call. It reads the options the page declares and compares them with the list the
  adapter crate keeps next to the code that builds the command
  (`review_runner_claude::CLAUDE_INVOCATION`, `review_runner_codex::CODEX_INVOCATION`); a unit test
  holds each list equal to what its command builder emits.
- The decision is made on the help page, never on a version string, so a wrapper or a fork is
  judged by what it accepts. Only a refused CLI is also asked for `--version`, to report it.
- For Codex, the check also proves the app-server protocol the identity and usage probes need: it
  starts the bare `codex app-server` in the same probe environment, sends `initialize`, accepts its
  answer and stops the process. Only a JSON-RPC response with id 1 whose `result` is a JSON object
  proves the protocol. It sends no account or rate-limit request, its answer is never a login
  answer, and it is bounded by the probe timeout. `af` starts `codex app-server` with no
  transport flag: `stdio://` is the default on every verified version, and `--stdio` does not
  exist on 0.121.0.
- No isolation flag is ever dropped, reordered or replaced to make an older CLI run. A CLI that
  lacks one is refused; there is no weaker fallback invocation.

### Claude Code

| What | Value |
|---|---|
| af invokes | `claude -p --output-format json [--model M --effort E] --safe-mode --restricted --permission-mode dontAsk --strict-mcp-config --tools … --allowedTools … [--json-schema …]`, prompt on stdin |
| Required options (`claude --help`) | `-p`/`--print`, `--output-format`, `--model`, `--effort`, `--safe-mode`, `--restricted`, `--permission-mode`, `--strict-mcp-config`, `--tools`, `--allowedTools`, `--json-schema` |
| Required protocol | `claude auth status --json` answers the identity probe |
| Checked | by `af provider status` and `af provider setup` for every context, and by Task Provider admission before any Worker or admission Attempt |
| Floor | 2.1.248, the first release with `--restricted` (`--safe-mode` arrived in 2.1.169), from the Claude Code changelog |
| Recommended | 2.1.295 or later: it fixed `--tools` and `--restricted` not applying to built-in tools that register after launch |
| Verified | 2.1.299 passes (macOS arm64; its `--help` and `--version` were rerun on 2026-10-14); 2.1.101 is refused, missing `--safe-mode` and `--restricted` (verified by hand on 2026-10-14) |
| Remediation | `npm install -g @anthropic-ai/claude-code@latest` |

### Codex

| What | Value |
|---|---|
| af invokes | `codex exec --ephemeral --skip-git-repo-check --json -C <sandbox> -s <mode> -o <file> [--model M -c model_reasoning_effort=…] -`, prompt on stdin |
| Required options (`codex exec --help`) | `--ephemeral`, `--skip-git-repo-check`, `--json`, `-C`/`--cd`, `-s`/`--sandbox`, `-o`/`--output-last-message`, `--model`, `-c`/`--config` |
| Required protocol | `codex app-server`, started bare, answers JSON-RPC `initialize` (id 1) over its default stdio transport, then `account/read` or `account/rateLimits/read` (id 2); `codex login status` answers the status probe |
| Checked | the options and the `initialize` answer by `af provider status` (with or without `--usage`), `af provider setup` and Task Provider admission; the whole protocol again by the identity probe before every private Worker send, and by `status --usage` |
| Floor | 0.121.0 verified; older not verified |
| Verified | 0.121.0 and 0.162.1 pass with the bare `codex app-server` (verified by hand on 2026-10-14: on a ChatGPT login `account/rateLimits/read` returns the rate limits, on an API-key login a JSON-RPC error `chatgpt authentication required to read rate limits`); 0.162.1's `codex exec --help` was rerun on 2026-10-14 |
| Remediation | `npm install -g @openai/codex@latest` |

A refused CLI is reported with its Provider ID and kind, the executable path, the detected version,
every missing option or protocol capability, and the upgrade command:

```text
af provider status: provider claude-main: incompatible Provider CLI: the claude CLI `/usr/bin/claude` (version 2.1.101 (Claude Code)) does not accept af's invocation; missing: --safe-mode, --restricted; fix: upgrade it with `npm install -g @anthropic-ai/claude-code@latest`, then rerun `af provider status`
```

| Where | What happens |
|---|---|
| `af provider status` | The context's `auth` is `incompatible` and its usability `unusable`, whatever its login. The command exits 4. stderr carries the report above, and the table's note repeats it. |
| `af provider setup` | `provider_cli_missing`, exit 4, before any login is asked about or started. The document's `diagnostic` is the af-authored summary, without the version; the report goes to stderr. |
| Task Provider admission | Refused before any Worker process starts and before any Attempt is reserved or charged: `Task Provider admission refused before any Worker was dispatched: provider …: incompatible Provider CLI: …`. A refused selection also names each candidate's reason after `No Pipeline selected: …` on stderr. |
| A Codex app-server that exits before `initialize` | The check reports `codex app-server exited 2 before answering initialize: error: unrecognized subcommand 'app-server'` — its exit status and the CLI's own first error line — and refuses the CLI as missing the protocol capability `` `codex app-server` answering JSON-RPC `initialize` over its default stdio transport ``. Plain `af provider status`, `status --usage`, setup and admission give the same `incompatible` report and exit 4; an app-server that answers `initialize` with an error is refused the same way, reported as `codex app-server exited 0 after answering initialize with an error: {"id":1,"error":{"code":-32600,"message":"bad init"}}`, and so is one whose answer has no `result` object (`{"id":1}`, a `null` or non-object `result`) or that writes a line that is not JSON-RPC, reported as `codex app-server exited 0 after answering initialize without a result object: {"id":1}`. Each carries its exit status when it exited, or `was stopped by af` when af stopped it still running, and the line it answered with. The identity and usage probes read the `initialize` answer by the same rule. When the CLI's own `--version` check also fails it is an installation failure instead. A check that times out leaves the context `unavailable`, never `authenticated`. Before a private Worker send the identity recheck refuses an app-server that stopped answering. |
| A Worker the CLI still refuses | A parsable result is, for Claude, a JSON object with a boolean `is_error` and its `result` (or, for a schema-constrained Attempt, its `structured_output`), and for Codex the `-o` final message of a process that exited 0 without an `error` or `turn.failed` event. Anything else is none: `{}` or other JSON on Claude's stdout, a missing final-message file at exit 0, stdout holding only unrelated events, a non-zero exit after `turn.failed`. When a Worker process exits without a parsable result, the Attempt diagnostic carries its exit status and a bounded stderr excerpt, whatever the failure is classified as (`network`, every `auth_*`, `quota`, `capacity`, `model_unavailable`, `unknown`, `unsupported_invocation`), and keeps the classification: `Codex Worker failed: Provider network request failed (network); it exited 1 without a parsable result; stderr: network failed: proxy refused connection`. The excerpt is read before the privacy filter replaces the captured stream. When the CLI refused af's own arguments (`unknown option`, `unexpected argument`, `unrecognized …`, a `Usage:` block), the failure is classified `unsupported_invocation` and the diagnostic says the installed CLI does not accept af's invocation and names the upgrade. |

The help page, the version, the CLI's error line and every stderr excerpt are Provider-authored
text. They are bounded and stripped of terminal escapes and control characters. One shared
credential filter judges every such line before it reaches stderr, the human table, a probe error or
an Attempt diagnostic, and replaces the whole line with `[line withheld: looks like credential
material]` when it contains:

- a key, token, secret, password, credential or auth assignment, in any case and with any prefix:
  `API_KEY=…`, `OPENAI_API_KEY="…"`, `ANTHROPIC_API_KEY: …`, `"refresh_token": "…"`;
- `Bearer` followed by a token, or an `Authorization` header;
- a token prefix such as `sk-`, `sk-ant-`, `sk-proj-`, `ghp_`, `github_pat_`, `xoxb-` or `glpat-`;
- a run of 32 or more hexadecimal characters, a run of 32 or more base64 or base64url characters
  (`A-Z`, `a-z`, `0-9`, `+`, `/`, `=`, `_`, `-`) that holds at least one upper-case letter, one
  lower-case letter and one digit, or a JSON Web Token (exactly three non-empty base64url runs
  joined by single dots, the first starting with `eyJ`, the first and third each 16 or more
  characters long, bounded by any other character or the line's ends; `error: eyJabc.def.ghi`
  passes), while a run of letters alone, such as a long option name or a
  path (`--dangerously-bypass-approvals-and-sandbox`, `/Users/alice/projects/af`), passes by
  design.

An ordinary argument error such as `error: unexpected argument '--stdio' found` passes through. A
stream that carries login-challenge material anywhere (an official login origin, a device or user
code, an OAuth parameter) is that marker alone, and so is a Worker's stderr excerpt when its stdout
carries such material, because both captured streams are then replaced. The exit status and the classification always
stay. Provider-authored text never reaches a versioned JSON document or retry feedback.

## Output and exit codes

stdout carries the result — human lines, or exactly one JSON document under `--json`. stderr
carries progress, warnings, and one diagnostic line for any non-zero outcome, so a pipeline that
discards stdout still shows the operator what happened.

| Code | Condition | Commands |
|---|---|---|
| 0 | success | all |
| 1 | unclassified failure (unsafe auth directory, unreadable registry, an ID `remove` does not find, …) | all |
| 2 | usage error, from clap | all |
| 3 | human action required | `setup` |
| 4 | Provider CLI missing, cannot start, or does not accept af's invocation | `setup`, `status` |
| 5 | registry conflict | `setup`, `add` |
| 6 | authentication failed | `setup` |
| 7 | optional usage unavailable | `status --usage` |

The versioned documents are [`provider-status-v2.json`](../schemas/provider-status-v2.json) and
[`provider-setup-v1.json`](../schemas/provider-setup-v1.json). Both are closed vocabularies. Neither
can represent an account email, an organization or account identity, a credential, OAuth material,
or raw Provider output — quota windows carry only the numbers `af` derived itself, and the human
table keeps the Provider's own window labels.

## Troubleshooting

| Symptom | Meaning and action |
|---|---|
| exit 3, `human_action_required` | Run the printed `af provider setup … --login` yourself at a private terminal. Never paste the OAuth URL or code anywhere else. |
| exit 4, `provider_cli_missing` or `installation_failed` | The official CLI is missing, not executable, or cannot start. Run the fix printed on stderr (for npm installs, usually the CLI's own reinstall command), then rerun `af provider status`. |
| exit 4, `incompatible` | The CLI starts but is too old for af's invocation. Run the upgrade printed on stderr, then rerun `af provider status`. |
| exit 5, `registry_conflict` | The ID or the auth directory already belongs to another entry. Choose a different ID, or point `--auth-dir` at the context you meant. |
| exit 6, `authentication_failed` | The Provider CLI answered, and not with a usable login. Repeat the login at a private terminal and confirm with `af provider status`. |
| exit 7 from `status --usage` | Optional usage probing did not answer. Authentication is unaffected; drop `--usage` if you only needed that. |
| `auth directory … is writable by another user` | Tighten ownership and permissions on the directory (and its rename-controlling parents) before registering it. |
| `provider registry … is invalid` after an auth directory was deleted | Run `af provider remove ID` for the entry that names it; name every stale entry in one command. |
| `unfinished publication transaction` | Run `af provider recover`: it validates the marker and every preserved hash before archiving, and deletes no version. |
