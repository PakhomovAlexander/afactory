# ADR-0130: Report a Provider CLI that cannot start as an installation failure

**Status:** accepted (2026-10-02). Extends
[ADR-0112](0112-refuse-agent-mediated-provider-logins.md): `af provider status` gains exit code 4
and the `installation_failed` authentication state.

## Context

On 2026-09-26 an auto-update left `@openai/codex` without its platform package. Running `codex`
failed at once with `Error: Missing optional dependency @openai/codex-darwin-arm64. Reinstall
Codex: npm install -g @openai/codex@latest`. Codex-backed Task Workers failed around the same time
with `unexpected status 401 Unauthorized`. Reinstalling fixed both, but af gave no clear signal
that the CLI itself was broken, and Attempts were spent before a human found the cause
([issue #136](https://github.com/PakhomovAlexander/afactory/issues/136)).

Every af probe of a Provider CLI asks a specific question: `codex login status`, `claude auth
status --json`, or an app-server `account/read`. A CLI that cannot start answers none of them.
af reported that silence as whatever the question was about. Status called the context
`unavailable` ("login status exited unsuccessfully"), setup called it `authentication_failed`, and
the Task identity recheck called it "Captured Task Provider identity is no longer current or could
not be verified". Each of these blames the login or the account.

## Decision

A Provider CLI **cannot start** when its program is not on PATH, when a file of that name is on
PATH without an execute bit, when the operating system refuses to start it, or when it exits
non-zero on its own `--version` check. Each is a **Provider installation failure**. The report
names the Provider ID, the program path, the CLI's own first error line and the fix the CLI
suggests. When the CLI suggests none, af falls back to "reinstall the official CLI".

The version check is diagnostic. af runs it only after its own probe got no answer it recognizes,
meaning the probe could not be spawned or exited non-zero with output af cannot parse. It never
runs when the CLI answered: a logged-out context or another account has started, whatever it
thinks of `--version`. It never runs when af gave up waiting, through a timeout, cancellation or
an elapsed Attempt deadline. Those prove nothing about installation, and a second bounded wait
would only double the first. A missing or non-executable program needs no process at all.

- `af provider status` reports the context with `auth: installation_failed` and `usability:
  unusable`, and exits 4. That code already means "Provider CLI missing" for setup. It now means
  "missing or cannot start" for both commands, and it outranks 7, because a broken CLI matters
  more than an unanswered optional usage probe. stderr carries one diagnostic line per broken
  Provider.
- `af provider setup` reports `provider_cli_missing` (exit 4) instead of `authentication_failed`.
- Task Provider admission refuses the binding while it binds models. That happens before
  any Worker is dispatched and before any Attempt is reserved or charged. The refusal is an
  ordinary command error.
- The identity recheck before each private Worker send separates a CLI that cannot start from
  an identity that changed. The Attempt still fails with zero charge and the existing
  `provider_failure` feedback code. Its diagnostic now reads `Provider environment failure, not a
  model or credential failure: …`. A captured executable that was removed is first replaced by
  the installed client, and refused by name when none is installed
  ([ADR-0126](0126-keep-the-captured-native-executable-when-its-launcher-moves.md)). The
  diagnosis applies to the client actually rechecked.

The CLI's error line and suggested fix are Provider-authored text. Each is bounded to one line,
with terminal escapes and control characters removed. They reach human output, stderr and Attempt
diagnostics. The versioned documents still carry no Provider-authored text: status carries only
the new closed state, and the setup diagnostic carries only the af-authored summary. Retry feedback
is never derived from the diagnostic
([ADR-0066](0066-reserve-task-attempts-before-binding-exact-context.md)).

## Considered options

- **Run `--version` before every probe.** Rejected. It adds a process to every status row and
  every admission. It also makes a CLI that answers its real protocol but rejects `--version`
  look broken, and the existing test doubles and some wrappers do exactly that.
- **Classify any failed probe as an installation failure.** Rejected. A logged-out context, a
  changed account and a slow machine would all be blamed on the installation. That is the same
  misdirection in the other direction.
- **Add a new exit code for status.** Rejected. Exit 4 already names this condition for setup,
  and one code per condition keeps the ADR-0112 table small.
- **Put the CLI's error text in the JSON documents.** Rejected. Both documents promise never to
  carry Provider-authored text. The closed `installation_failed` state is the machine-readable
  signal, and stderr carries the words.
- **Re-diagnose after every failed Worker invocation.** Rejected. The recheck before the send
  already runs immediately before the native client. A version check after every failed model
  call would also misclassify test doubles and spend a process on ordinary model failures.

## Consequences

- An operator whose CLI broke sees the CLI's own reinstall command from `af provider status`,
  setup, or the Task command that refused. No Attempt is spent before they do.
- A CLI that breaks mid-Task fails its remaining Attempts at zero charge, and reports say why. The
  diagnostic names neither the model nor the login.
- `provider-status-v1.json` admits `installation_failed` and exit code 4. Every document valid
  before this change is still valid, and every existing document for a working CLI is unchanged.
- A status run on a machine whose registered Provider has no CLI installed now exits 4 instead of
  0. That outcome is the documented new failure kind.
