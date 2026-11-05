# ADR-2026-11-05-publish-owned-impl-directory-reservations: Recover directory reservations

**Status:** proposed (2026-11-05).

## Context

The impl lifecycle wrote directory ownership after creating final worktree and build paths.
A process interruption between mkdir and the journal save stranded a legitimate preparation:
recovery correctly refused the existing directory without a durable inode identity.

## Considered options

- Adopt empty final directories with null ownership: rejected because emptiness proves no ownership.
- Journal intent and then mkdir at the final path: rejected because intent cannot prove which
  process created a directory after interruption.
- Use ordinary rename from staging: rejected because rename may replace a foreign empty directory.
- Allocate unlimited staging names: rejected because repeated crashes would leak unbounded artifacts.
- Delete unjournaled staging on restart: rejected because its ownership is uncertain.

## Decision

For both directory kinds, create private staging under the configured worktree root, fsync it
and its parent, persist its device/inode, then atomically publish with exclusive rename.
Require native Linux renameat2 RENAME_NOREPLACE or macOS renamex_np RENAME_EXCL; fail closed
elsewhere. Fsync both rename parents before persisting publication completion. Under the
existing journal lock, recovery accepts only the recorded inode, at staging or the final path;
a missing recorded directory, changed content or identity, or foreign final path is refused.
Git checkout remains separate and partial registrations or dirty checkouts are never reset.

Use four deterministic staging slots per kind and intent. An empty private directory found
without a saved identity remains unclaimed and untouched. Recovery durably advances the slot
before creating another. At most three uncertain slots can be bypassed; the fourth exhausts
reservation authority. Nonempty or symlinked uncertain staging is refused. Normal cleanup
continues to remove only explicitly owned final artifacts; uncertain staging remains for
operator inspection. Repeated restarts cannot grow the bounded set.

Advance only the implementation journal to af/scenario-impl-state@2. Update its writer, reader,
shipped schema registration, schema generator and parity/rejection tests together; reject v1.
Public native Scenario interfaces, signed publication authority and merge/cleanup checks stay
as defined in ADR-2026-11-05-bind-implementation-lifecycle-to-native-scenario-authority.

## Consequences

Preparation resumes the same native intent across mkdir, fsync, identity-save, exclusive-rename
and publication-save interruptions for both worktree and build directories. Ownership never
comes from a null field or emptiness. A fixed number of uncertain empty directories may remain
for operator inspection; exhaustion requires operator action and never silently completes.
The real-Git regression matrix checks both sides of each boundary and preserves foreign,
changed, nonprivate, partial and symlinked artifacts. No remote effect is needed for these tests.
