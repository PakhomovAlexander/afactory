# ADR-0243: Add pinned Task authority through additive onboarding

**Status:** proposed (2026-10-27).

## Context

ADR-0032 made review onboarding absent-only. A repository already onboarded for review
had no supported path to a model-driven Task catalog without assembling packages, policy
and digests by hand. The Task runtime reads authority only from a committed Snapshot.

## Decision

`af onboard --tasks` previews a token-free Task catalog, code policy, package files and
the exact `[task.defaults]` text. It uses the same Gate discovery as review onboarding,
or explicit `--gate NAME=COMMAND`. Packages are pinned with the same digest recipe as
`af catalog digest`. Distinct models give implementer and independent reviewers separate
model identities even when one Provider login serves them.

With no `.af/`, `--apply` installs review and Task authority as one absent bundle. In
an existing `.af/`, it adds only absent Task catalog, code policy and packages and appends
the absent `[task.defaults]` table to `.af/af.toml`. Any existing target, including a
`[task]` table, refuses the entire operation. It never merges existing authority or
commits the result. The additive apply stages a complete authority tree, retaining
existing regular files through hard links, then exchanges the staged and original
directories atomically. The repository owner reviews and commits the diff before it
becomes execution authority. This amends only ADR-0032's absent-only boundary.

## Considered options

- Keep hand-built Task policy: rejected because package pins and contracts are difficult
  to reproduce correctly on a first run.
- Stage a bundle outside `.af/` for a manual patch: rejected because the owner chose an
  explicit additive apply with complete preflight.
- Reuse an existing code policy: deferred because silently accepting an unrelated check
  set could weaken Task acceptance.
- Replace or merge existing Task files: rejected because existing authority is project
  owned and must be reviewed explicitly.

## Consequences

An onboarded repository can acquire a pinned model-driven Task path without changing
its existing review files' bytes. The generated files remain untrusted until committed.
The command refuses partial Task authority and preserves the original review onboarding
and lock refresh contracts.
