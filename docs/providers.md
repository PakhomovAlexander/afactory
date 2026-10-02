# Providers

How a machine gets the model access `af review` and `af task` need, and what that flow promises a
caller that is not a human sitting at a terminal.

A **Provider** is an operator-named, machine-local authentication context for one adapter kind
([`../CONTEXT.md`](../CONTEXT.md)). `af` never holds a credential: the official `claude` or `codex`
CLI owns it, and the machine-local registry at `~/.config/af/providers.toml` records only an ID, a
kind, and an auth directory. Bootstrap runs in the invoking release rather than a repository's
pinned one, and registry publication stays atomic for older readers
([ADR-0111](adr/0111-keep-provider-bootstrap-machine-local-and-cross-release-safe.md)).

## The one rule that shapes everything else

An interactive Provider login prints an **OAuth URL and an authorization code**. Those are
credentials in transit. Anything that can read them — a pipe, a chat window, an agent transcript, a
CI log — is a place they must never reach.

So `af provider setup` starts an official login only when **both** are true:

1. the operator passed `--login`, and
2. this process owns an interactive terminal on stdin, stdout **and** stderr.

Otherwise setup fails fast with the `human_action_required` result, exit code 3, and the exact
command a human should run at a private terminal. `af` does not proxy, broker, or headlessly
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

An agent, a CI job, or anything else reading `af`'s output should run `setup` **without**
`--login`, read the result, and surface the `next_action.command` to a human.

## Checking a Provider

```sh
af provider status                 # fast: the registry plus one authentication check per context
af provider status --json          # the same, as af/provider-status@1
af provider status --usage         # additionally probe subscription and quota windows
af provider doctor --campaign NAME --provider correctness=codex-main
```

`status` is deliberately cheap and says only what it checked. It reports four independent axes:

| Axis | Values | Means |
|---|---|---|
| `registered` | true / false | in the registry, so selectable by `--provider NODE=ID`; false is an ambient discovery label |
| `auth` | `authenticated`, `not_authenticated`, `unknown`, `unavailable`, `installation_failed`, `not_probed` | what the Provider CLI said about this auth directory; `installation_failed` means the CLI cannot start, so it said nothing |
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

## Output and exit codes

stdout carries the result — human lines, or exactly one JSON document under `--json`. stderr
carries progress, warnings, and one diagnostic line for any non-zero outcome, so a pipeline that
discards stdout still shows the operator what happened.

| Code | Condition | Commands |
|---|---|---|
| 0 | success | all |
| 1 | unclassified failure (unsafe auth directory, unreadable registry, …) | all |
| 2 | usage error, from clap | all |
| 3 | human action required | `setup` |
| 4 | Provider CLI missing or cannot start | `setup`, `status` |
| 5 | registry conflict | `setup`, `add` |
| 6 | authentication failed | `setup` |
| 7 | optional usage unavailable | `status --usage` |

The versioned documents are [`provider-status-v1.json`](../schemas/provider-status-v1.json) and
[`provider-setup-v1.json`](../schemas/provider-setup-v1.json). Both are closed vocabularies. Neither
can represent an account email, an organization or account identity, a credential, OAuth material,
or raw Provider output — quota windows carry only the numbers `af` derived itself, and the human
table keeps the Provider's own window labels.

## Troubleshooting

| Symptom | Meaning and action |
|---|---|
| exit 3, `human_action_required` | Run the printed `af provider setup … --login` yourself at a private terminal. Never paste the OAuth URL or code anywhere else. |
| exit 4, `provider_cli_missing` or `installation_failed` | The official CLI is missing, not executable, or cannot start. Run the fix printed on stderr (for npm installs, usually the CLI's own reinstall command), then rerun `af provider status`. |
| exit 5, `registry_conflict` | The ID or the auth directory already belongs to another entry. Choose a different ID, or point `--auth-dir` at the context you meant. |
| exit 6, `authentication_failed` | The Provider CLI answered, and not with a usable login. Repeat the login at a private terminal and confirm with `af provider status`. |
| exit 7 from `status --usage` | Optional usage probing did not answer. Authentication is unaffected; drop `--usage` if you only needed that. |
| `auth directory … is writable by another user` | Tighten ownership and permissions on the directory (and its rename-controlling parents) before registering it. |
| `unfinished publication transaction` | Run `af provider recover`: it validates the marker and every preserved hash before archiving, and deletes no version. |
