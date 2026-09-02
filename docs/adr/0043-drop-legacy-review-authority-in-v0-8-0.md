# Drop legacy `.review/` authority in v0.8.0

**Status:** accepted (2026-09-02)

From release `v0.8.0`, `af review plan`, `af review run`, and `af onboard` accept review
authority only under `.af/`. The legacy layout — `.review/pipelines/`, `.review/reviewers/`,
`.review/review.lock` — is no longer read for new Campaigns or Tasks. Stored Campaign state whose
Authority Snapshot pinned `.review/...` paths remains replayable: the authority layer keeps
resolving those recorded paths, because a pinned Campaign must never acquire new policy.

This retires an authority *layout*. It renames nothing persisted: `review.kernel/*` artifact
types, persisted events, schemas, CAS content, and established Review Kernel domain terms stay
frozen exactly as before. The AGENTS.md rule that no rebranding may rename `.review/` until a
migration ADR supersedes it is superseded by this decision for the layout only.

Consumers move with one command. `v0.8.0` ships `af onboard --migrate` converting a
`.review/` repository into `.af/` — project file, lock with Worker and pipeline pins and the
writing release's version, pipeline with format upgrades applied, Worker packages copied byte for
byte — as a preview first and atomically on `--apply`. The consumer reviews the diff, commits it,
and deletes `.review/`. Implementation is tracked in [#52](https://github.com/PakhomovAlexander/afactory/issues/52).

## Considered options

- **Keep both layouts indefinitely.** Rejected: two authority code paths, two lock shapes, and a
  dual-layout smell the product audit flagged; the owner sees no value in carrying it.
- **Drop the layout without a move command.** Rejected: consumers would hand-migrate digests and
  lock entries, exactly the class of error onboarding exists to remove.
- **Deprecate by date.** Rejected: releases are the unit consumers pin; a date means nothing to a
  launcher pinned to a version.
- **Drop in the next release and ship the move command with it (chosen).** In-place upgrades from
  issue #46 keep 0.7.x consumers working until they move.

## Consequences

- The in-place `.review/` upgrades (issue #46) remain for consumers on 0.7.x; they are not a
  destination.
- The Afactory hub moves to `.af/` before pinning `v0.8.0`; the release-CI consumer fixture
  switches to the `.af/` layout at the same time, so the release workflow proves the layout
  consumers actually pin.
- `v0.8.0` release notes name the change and the one migrating command.
- Later work may retire the `.review` branch of the authority layer only after every stored
  Campaign that could be replayed has aged out of support — a separate decision.
