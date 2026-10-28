# ADR-0254: Leave out a default context that holds a registered login

**Status:** proposed (2026-10-28). Narrows further the rule
[ADR-0141](0141-list-a-logged-out-default-context-only-until-its-kind-is-registered.md) records
for the ambient discovery labels of
[ADR-0136](0136-remove-a-registered-provider-by-id.md).

## Context

`af provider status` and the browser's Providers pane list the context each Provider CLI uses by
default (`CLAUDE_CONFIG_DIR` or `~/.claude`, `CODEX_HOME` or `~/.codex`) as `claude-ambient` or
`codex-ambient` while it is not a registered Provider's auth directory. ADR-0141 leaves such a
context out once a Provider of its kind is registered, but only when it holds no login: a default
context with a login stays, as a login the operator may still want to register.

That rule assumed one login lives in one directory. The Claude CLI keeps its credentials apart
from its configuration directory: on macOS they are a Keychain item, and the item the default
directory reads is the one the operator's `claude login` wrote before they moved that login into
a directory of its own. An operator who registered `claude-personal` on `~/.config/claude-personal`
and `claude-work` on `~/.config/claude-work`, and whose shell sets `CLAUDE_CONFIG_DIR` only inside
the aliases that start each CLI, sees a third row in every status table and in the browser:
`claude-ambient`, authenticated, the same account and organization as `claude-personal`, with
nothing to remove behind it. It reads as a login they forgot, and it is not one.

## Decision

A default context whose login is one a registered Provider of its kind already holds is left out
of the inventory, as a logged-out one is.

- **The login is what the status answer names.** For Claude, `auth status` reports the account
  and the organization; both must match, since one account in two organizations is two logins.
  The Codex answer names no account, so the rule never applies to a Codex default context; its
  directory is compared as a path, as before.
- **The login never leaves discovery.** The row and the login it answered with travel together
  only inside `discover_with_cancel`; `ProviderStatus`, the JSON status document and the browser
  carry no account label, as the status document's rule has it: no account email, organization
  or plan-holder identity on any status surface.
- **A context left out is not a sibling.** The logged-in-sibling notes of a logged-out Provider
  are written after the drop, so no note points at a row that is not listed.
- **Everything ADR-0141 keeps, stays.** A default context with a login of its own, one whose
  kind has no registered Provider, and one in any other state are listed as before.

## Considered options

- **Compare the directories.** Rejected: the directories differ; the credential store behind
  them is what they share, and af does not read credential stores.
- **Compare the account alone.** Rejected: the same account in another organization is a login
  the operator has not registered, which ADR-0141 lists on purpose.
- **Say on the row that it duplicates a registered Provider.** Rejected: the row would still
  read as a fault in `af provider status`, ADR-0141's reason for leaving the logged-out one out.
- **Ask the operator to log the default directory out.** Rejected as the only answer: it is the
  Claude CLI's own default, and the next `claude login` without the variable brings it back.

## Consequences

- An operator whose registered Providers hold the logins their default directories do no longer
  sees a `claude-ambient` row in `af provider status`, `--json` or the browser.
- The names-only scan of [ADR-0206](0206-detect-provider-logins-and-register-them-through-add.md)
  still names the directory: `found but not registered: ~/.claude (not probed) → af provider
  detect`, as it does when the variable is set; `af provider detect` still lists it.
- Logging the default directory in to another account, or logging the registered one out, brings
  the row back.
