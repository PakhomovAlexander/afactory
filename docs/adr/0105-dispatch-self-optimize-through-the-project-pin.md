# ADR-0105: Dispatch `self optimize` through the project pin

Date: 2026-09-17
Status: Accepted
Supersedes the command-wide `self` dispatch exemption in
[ADR-0044](0044-af-manages-itself-and-dispatches-to-the-pinned-release.md) for `self optimize`
only.

## Context

Binary-management commands must run outside project pins so an old project release cannot block
install, update or rollback. Project optimization has the opposite authority: it captures the
project's Task catalog, history policy and exact release bytes, and its result must replay under
those same bytes. Treating all `self` subcommands alike would execute optimization using the
default binary even when the project selected another release.

## Decision

Make the dispatch exemption command-specific. `af self status`, `update`, `rollback`, `install`,
`remove`, `prune`, `setup-shell`, `uninstall`, `man` and the internal refresh command remain
exempt. `af self optimize` resolves `--repo` before dispatch and follows the normal project-pin
path. A pinned release that does not implement optimization refuses; the default binary never
runs the request as a fallback.

The initial command captures and previews an ordinary report-only Task. Preview and initial
confirmation follow accepted ADR-0104. Dispatch grants no history-source, model, delivery,
publication or configuration-write authority: those remain explicit captured Task inputs and
effects. `AF_DISPATCHED_FROM` continues to fence recursive dispatch.

## Considered options

- Keep the whole `self` namespace exempt: rejected because reports and Task state would not bind
  the project's selected engine bytes.
- Rename the feature outside `self`: rejected because the owner-facing spelling is established
  and command-specific dispatch is unambiguous.
- Let a new default binary emulate optimization for old pins: rejected because that silently
  replaces captured project authority and breaks replay.

## Consequences

Argument inspection must distinguish `self optimize` before the normal command parser runs and
must resolve its repository selector exactly like Task commands. Fixtures cover the dispatching
case, every unchanged binary-management exemption and the unsupported-old-pin refusal. Future
project-scoped `self` subcommands require their own accepted decision; this exception does not
generalize automatically.
