# ADR-2026-11-15-authenticate-wake-continuations-and-pin-attachment-authority: Seal wake authority

**Status:** proposed (2026-11-15); candidate repairs for #958, #959 and #960.

## Context

A recomputable checkpoint checksum plus checked history does not prove retained lifecycle
semantics. Rebuilding after new cancellation facts can erase corruption without inspecting it.
Pi also validated fresh configuration while executing captured configuration. Executable file
ownership alone did not protect its pathname through writable directory ancestors.

## Decision

Authenticate each exact retained checkpoint with HMAC-SHA256 and a private, random, 32-byte
Store key outside SQLite. The closed `af/task-wake-checkpoint-seal@1` replaces the former bare
checksum in the control column; earlier unsealed state fails closed without migration. The
checkpoint payload shape stays unchanged. The authentication material includes the complete
serialized checkpoint, exact registration, Store incarnation and both persisted progress columns.
The trusted kernel creates a seal when publishing a checked projector page in the cursor/outbox
transaction. Seals are internal provenance, never a public operation accepting caller summaries.

Initial serialized inspection, retained restoration, atomic replacement and fresh contention
validation verify the actual retained object. This applies before comparing current recorder
facts and before choosing opening replay. Changing semantics, counts, facts, clocks or any other
field cannot reuse a seal for another object or registration. Canonical source/CAS validation,
exact rolling prefix and latest lease validation remain independent requirements.

The key is owner-private, single-linked, bounded, no-follow and durably published before Store
incarnation admission. One fixed bounded staging file permits recovery before admission; missing
or corrupt keys after admission refuse. Reopening reestablishes the file/directory durability
barrier. Both names are ordinary accounted Store entries; notification collection requires no
new CAS references and retains no historical recorder-facts archive. The finite control allowance
includes both 32-byte files. This is protection from mutable-row corruption, not from a malicious
same-UID owner who can read the key or modify installed kernel code. Destructive Store removal
ends provenance recovery. A copied database still fails the existing incarnation ownership check.

Every delayed Pi launch requires fresh validated configuration equal to every captured authority
field, including executable pathname/digest and route/repository/Store selectors. Delayed callbacks
repeat that check before native follow-up. Drift invalidates the attachment; a potentially committed
native intent remains uncertain. Poll count, wall time, subprocess bounds and generations stay fixed.

Native installation admission and revalidation, and Pi's AF executable validator, inspect the
entire canonical executable ancestry. Directory owners must be root or the current owner. Group
or other writable directories require sticky protection, with the next child also trusted-owned.
Every ancestor must remain a directory without symlinks; the final regular file is
trusted-owned and not writable by group/others. Hard links do not grant another UID write access;
the invoked pathname and its ancestry must be protected independently of any other names. Hashing and invoking use the same canonical path.
A trusted sticky `/tmp` is allowed. A foreign-owned sticky directory is insufficient because its
owner can remove children. Canonical owner-private socket parents and mode-0777 Codex sockets
keep their separate endpoint rules. No additional platform support is claimed.

## Considered options

- Trust checksums or only replay source references: rejected; derived fields remain forgeable.
- Retain all prior facts and observation clocks for full reconstruction: unnecessary storage and
  lifecycle complexity when an authenticated exact writer attestation suffices.
- Discard and rebuild corrupt checkpoints: rejected; corruption must remain terminal and visible.
- Accept fresh Pi configuration while executing old values: rejected; validation must bind use.
- Check only the executable's immediate directory: rejected; higher components can be substituted.

## Consequences

Facts changes still rebuild valid old continuations. Invalid continuations cannot change progress,
create delivery effects or become retryable through a valid concurrent writer. Key loss refuses
recovery; there is no automatic key replacement for an admitted Store. The signature authenticates
kernel-written state, not external delivery, turn start or report evidence.

Accepted research remains Document
`sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`,
source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`.
Its common Store/outbox and conservative uncertainty recommendation is preserved. Original reviewed
comparison base remains `1a4dd4cc82d5f01f55014283c95d3bf6ea77fac3`; this repair's supplied source
binds PR #879 commit `5459467c75632b8d243a1be109f0788c1fa11b30`. Independent candidate verification
and live Pi/Codex certification remain coordinator work; Claude live trials remain deferred #876.
