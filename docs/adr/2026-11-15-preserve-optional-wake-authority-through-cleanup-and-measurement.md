# ADR-2026-11-15-preserve-optional-wake-authority-through-cleanup-and-measurement: Preserve wake authority

**Status:** proposed (2026-11-15); candidate repairs for #965, #966 and #967.

## Context

The fixed OS-account authority introduced for #963 must survive supported self-management
operations. Purging the default state root could erase its binding while a different Task Store
retained uncertain native admission. Account lookup also made ordinary storage initialization
fail for an unmapped numeric UID. Private HOME/XDG environments alone did not isolate benchmark
children from the actual account authority, and a guarded telemetry builder stripped the debug
fixture marker before its final AF child.

## Considered options

- Copy and restore authority around purge, or check that it is empty before deletion: rejected.
  Neither preserves concurrent first grants or the original uncertainty fence.
- Ignore every account lookup or inventory error: rejected. Corrupt existing authority, lookup I/O
  failures and accounting failures must retain their existing refusal/reporting behavior.
- Use HOME/XDG as a production authority fallback or honor fixture overrides in releases: rejected.
  Either would reopen the cross-Store namespace bypass.
- Preserve reserved paths, distinguish absent capability, and isolate measurement accounts outside
  the measured AF interval: selected.

## Decision

A purge refuses before any teardown whenever the reserved account authority exists, including an
incomplete directory, a file or a substituted link. Uninstall without purge remains available.
Independently, every whole-tree self-management/storage removal refuses an overlap with the
reserved authority, using both its reserved spelling and resolved destination. When authority is
absent, root purge removes unrelated children but never removes reserved ancestor directories.
This unconditional reservation closes the first-grant race without an empty-check/deletion lock,
copying bindings, resetting fences or inventing another namespace. Installation replacement and
abandoned staging cleanup follow the same reservation; shell-file cleanup cannot unlink inside it.

A missing OS account, or an account home that is relative or root, yields unavailable wake
capability. Ordinary Task planning, execution and storage initialization continue with their valid
private HOME/XDG/Store configuration. Explicit grant refuses clearly before writing a grant.
OS-account lookup I/O errors and identity mismatches remain errors. An existing authority still
participates in inventory and accounting; no authority read, permission or corruption error is
converted into absent capability. Production never substitutes HOME/XDG for account identity.

Telemetry's guarded final-child environments retain only a fixture marker matching the canonical,
owner-private invoking HOME. They inherit no other AF configuration. The debug override remains
absent from release authority selection. Standalone Linux measurement scripts, the local telemetry
executor and release smoke use a private user/mount namespace with a private files-only NSS account
database. The actual effective UID's account home therefore points into private measurement state.
Namespace setup surrounds the outer script, before AF child timing starts; it does not wrap each
measured AF invocation, replace production lookup/accounting, alter counters or change the measured
executable. Missing namespace support refuses without an unisolated fallback. Contained telemetry mounts the same private passwd/NSS files read-only at its existing container
boundary, preserving its limits and requiring no nested namespace or new capability. Other contained
measurements retain their separate container OS/account and filesystem boundary. On macOS, where
AF has no OS-account wake authority, this Linux overlay is unnecessary.

The accepted research Document
`sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`, from
source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`,
recommends durable shared subscriptions/outbox and conservative uncertain-admission handling.
These repairs preserve that recommendation. They grant no execution or publication authority and
provide no new Pi/Codex live certification; Claude live remains deferred.

## Consequences

Purge can leave recovery state or refuse explicitly; it cannot make an uncertain session fresh.
Containers running a numeric UID without a passwd entry can use ordinary Tasks without wake.
Linux local release measurements require Bubblewrap and permitted user namespaces, independently
of debug fixture support. Deterministic tests inject only libc account lookup, exercise final AF
children with an external-account open tripwire, and preserve database/key/WAL/SHM bytes. Public
CLI fixtures cover cross-root uncertainty, purge versus first grant, and ordinary no-wake cleanup.
No persisted or public wire shape changes, compatibility reader, model call or live host trial is
introduced.
