# ADR-2026-11-11-create-private-runtime-directories-and-explicit-permission-fixtures: Make directory modes explicit

**Status:** proposed (2026-11-11).

## Context

Issues #766 and #728 exposed fixtures whose directory modes depended on the invoking umask.
Provider fixtures created with tempfile's default mode became group writable under `0002`, so
Provider directory safety correctly refused them. The triage fixture's requested `0755` became
private under `0077`, so its public-directory refusal assertion never exercised a public root.
Check and Attempt roots and the warm-cache parent also used default creation modes despite
their intended private ownership. Issue #767 exposed real Provider discovery in a clean-host
doctor fixture; the current fixture already supplies an isolated tool PATH.

## Options considered

- Relax Provider directory safety or the public-root assertion: rejected because these
  refusals enforce the existing security boundary.
- Set a process-wide umask or chmod user-selected directories: rejected because caller
  permissions and unrelated directories are outside this repair's authority.
- Explicitly create private af-owned directories and permission-specific fixtures: chosen.

## Decision

Create fresh sandbox, Attempt and check roots with requested mode `0700`; create check children
and missing warm-cache parents with that mode too. Preserve existing owner-access restoration
for check children and the descriptor-based cache validation and cleanup rules. Creating cache
parents does not chmod existing ancestors.

Provider tests share explicit private directory and `0600` file constructors. File writes retain
the permissions of existing files. Fixtures testing unsafe modes
continue to set those modes explicitly and retain their refusal assertions. The triage test
explicitly chmods its own public fixture to `0755`. Keep doctor discovery on its private tool
PATH, with no real Provider CLI. Regressions run selected fixture children under varied umasks
and a PATH containing trap Provider launchers, without changing the runner's process umask.

## Consequences

Permission fixtures test their intended modes under both permissive and restrictive host
defaults. Provider safety validation, public/private assertions, contracts and schemas are
unchanged. Existing operator directories retain their permissions. No serialization,
performance, publication or Provider authority behavior changes.
