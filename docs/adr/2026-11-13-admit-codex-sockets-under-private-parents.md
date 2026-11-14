# ADR-2026-11-13-admit-codex-sockets-under-private-parents: Admit protected Codex sockets

**Status:** proposed (2026-11-13); candidate repair, live certification pending.

## Context

The [local wake delivery candidate](2026-11-13-drive-opt-in-local-task-wakes.md) rejects socket
group/other permission bits. The coordinator's Codex 0.167.2 deployment creates a mode `0777`
Unix socket within its owner-owned mode `0700` directory. That check rejects the demonstrated
deployment before delivery. No installed endpoint is accessed or changed by this repair.

## Considered options

- Require operators to chmod the daemon socket: rejected; the installed daemon owns its lifecycle.
- Accept sockets in public directories: rejected; endpoint authority requires a private parent.
- Admit owner-owned sockets behind the existing canonical private parent check: selected;
  Linux pathname traversal protects the endpoint independently of its socket mode.

## Decision

Allow an owner-owned Unix socket's group/other mode bits when its canonical owner-owned parent
has no group/other access. Preserve canonical paths without symlinks, socket type and ownership,
pinned device/inode, bounded connection, same-UID peer authentication, trusted executable digest,
exact original session/capability scope and all existing expiry and revocation checks.
No public shape changes, migration or network destination is introduced.

## Consequences

Recording fixtures cover mode `0777` grant, registration and drain, unsafe parent refusal at
grant and later use, symlink/inode substitution and foreign peer credential refusal. The existing
native admission uncertainty and no-resend assertions remain. Sandbox restrictions that prevent
binding a fixture socket remain test failures; they are not skipped or represented as passes.
Actual installed-host certification remains coordinator work after source verification.
See [operator setup](../task-wake.md) for the directory and socket distinction.
