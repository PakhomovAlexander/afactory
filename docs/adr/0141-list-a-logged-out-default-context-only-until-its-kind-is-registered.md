# ADR-0141: List a logged-out default context only until its kind is registered

**Status:** accepted (2026-10-05). Narrows when `af provider status` lists an ambient discovery
label, the rule [ADR-0136](0136-remove-a-registered-provider-by-id.md) records.

## Context

`af provider status` and the browser's Providers pane list `claude-ambient` and `codex-ambient`:
the context each Provider CLI uses by default (`CLAUDE_CONFIG_DIR` or `~/.claude`, `CODEX_HOME`
or `~/.codex`), computed on every run and listed while it is not a registered Provider's auth
directory. ADR-0136 made `af provider remove` refuse such a label, since no registry entry
stands behind it, and rejected a "hidden" flag in the registry.

An operator who registers each login under its own directory and starts `af` without the CLI's
variable set sees the default context as a row that is `not authenticated`, offers nothing to
remove, and stays. On the machine that raised this, `~/.claude` held an old logged-out
directory beside two registered Claude Providers. The row was noise that looked like a fault.

## Decision

A default context whose status is `not authenticated` is left out of the inventory once a
registered Provider of the same kind is listed.

- **Before any Provider of its kind is registered, it stays.** On a new machine the row and its
  `auth login` fix are how setup starts; without it `af provider status` would print nothing for
  an installed CLI.
- **A default context with a login stays.** It is a login the operator may register, and the
  logged-out-sibling note of a broken registry entry points at it.
- **Any other status stays.** `unavailable` or a failed installation is a CLI problem worth
  seeing.
- **Discovery output only.** Registry contents, Provider bindings and admission are unchanged;
  `af provider remove` still refuses an ambient ID by name when one is listed.

## Considered options

- **Hide every logged-out default context.** Rejected: a machine with the CLI installed and no
  Provider yet would list nothing and could not show the login fix.
- **Record a "hidden" flag.** Rejected for ADR-0136's reason: registry version 1 refuses
  unknown fields in every supported pinned release.
- **Keep listing it and explain on `d` in the browser.** Rejected as the only change: the row
  would still read as a fault in `af provider status`.

## Consequences

- An operator whose registered Providers cover their logins no longer sees the CLI's unused
  default directory in `af provider status --json` or the browser.
- Logging in to the default directory brings the row back.
