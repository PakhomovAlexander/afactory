# ADR-0136: Remove a registered Provider by ID

**Status:** accepted (2026-10-04). Extends
[ADR-0111](0111-keep-provider-bootstrap-machine-local-and-cross-release-safe.md): `af provider
remove` joins `setup` and `recover` as machine-local bootstrap that runs in the invoking release.

## Context

The Provider registry had `add` and `setup` and no inverse. Dropping a Provider meant editing
`~/.config/af/providers.toml` by hand, outside the lock and the atomic publication that ADR-0111
requires of every registry write.

The need is sharpest when an auth directory is deleted. Registry validation inspects every
entry's auth directory, so one missing directory makes the whole file invalid: `af provider
status` warns and lists no registered Provider, and `af provider add` cannot write either.

The browser lists Providers and offered no way to drop one. Its rule is that every mutation goes
through the path the CLI uses ([ADR-0119](0119-open-a-read-first-browser-on-bare-af.md)).

An ambient ID (`claude-ambient`, `codex-ambient`) looks like an entry in `af provider status` and
in the browser. It is not one: `af` computes it on every run from the directory the Provider CLI
uses by default, and lists it while that directory is not a registered Provider's.

## Decision

`af provider remove ID...` removes the registry entries that carry the named IDs.

- **Only entries go.** Each auth directory, and the login its Provider CLI keeps there, is left
  untouched. `af` holds no credential and deletes none.
- **Publication is the one `add` uses.** The command takes the registry lock, stages a candidate,
  validates it completely, and commits it with one atomic exchange. The registry it replaced is
  preserved in the recovery directory, and the command prints where.
- **The existing registry is read by shape alone.** It must be TOML with `version = 1` and a
  `providers` array of tables. Its entries are not validated before removal, so an entry whose
  auth directory is gone can be removed. The candidate is validated as every published registry
  is. A removal that would leave an invalid registry is refused and writes nothing. Several IDs
  may be named, so one command drops every stale entry.
- **The command is all or nothing.** An ID that is not registered refuses the whole command with
  exit 1. An ambient ID is refused by name, with the reason it is listed. An absent registry is
  not created, and neither is its directory or lock.
- **It runs in the invoking release.** `remove` is exempt from dispatch through `.af/af.lock`, as
  `setup` and `recover` are.
- **The browser offers the command, and does not run it.** `d` on a registered Provider, on the
  bar or in its opened pane, fills the `:` line with `provider remove ID`. Enter runs it through
  the hand-off of
  [ADR-0123](0123-hand-the-terminal-to-af-commands-typed-in-the-browser.md), and the Providers
  pane discovers again. `d` on an ambient candidate says there is no registry entry to remove.

## Considered options

- **Keep hand editing as the only way.** Rejected. A hand edit bypasses the lock and the atomic
  exchange, and the operator has to know the registry's TOML shape at the moment the registry is
  least readable.
- **Validate the existing registry before removal, as `add` does.** Rejected. The entry that
  makes the registry invalid is the one the operator most needs to remove.
- **Publish a candidate that is still invalid when the existing registry already was.** Rejected.
  ADR-0111 lets the live pathname hold only a complete, validated registry. Naming every stale
  entry in one command reaches a valid registry without weakening that rule.
- **Remove on a key in the browser, behind a yes/no prompt.** Rejected. It would be a second
  mutation path beside the CLI's. Filling the `:` line and waiting for Enter is the pattern the
  Tasks pane already uses for `task run`.
- **Let `remove claude-ambient` record a "hidden" flag.** Rejected. The registry is version 1
  and its readers refuse unknown fields, so a new field would make the registry invalid for every
  supported pinned release. The ambient row is discovery output, not state.
- **Dispatch `remove` through the project pin.** Rejected for the reason ADR-0111 gives: an older
  pinned release does not contain the command, and machine credentials are not project authority.
- **Also log out, or delete the auth directory.** Rejected. Credentials belong to the Provider
  CLI. The same directory may be the operator's everyday login.

## Consequences

- An operator drops a Provider with one command from any repository, including one pinned to an
  older release, and repairs a registry broken by a deleted auth directory the same way.
- `af` does not check whether a pipeline, Campaign or Task still binds a removed ID. Such a
  binding fails as it would after a hand edit, and registering the ID again restores it.
- A comment directly above a removed entry is part of that entry and goes with it. Every other
  byte of the file stays, and the previous version is preserved.
- `remove` adds no exit code, schema or `--json` document. `add` keeps its dispatch behavior.
