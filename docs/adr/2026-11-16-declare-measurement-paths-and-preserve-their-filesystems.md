# ADR-2026-11-16-declare-measurement-paths-and-preserve-their-filesystems: Declare measurement paths

**Status:** proposed (2026-11-16); consolidated repair for #976, #978–#982.

## Context

The required-branch launcher still conflated directories with fabricated `entry` filenames,
interpreted only standalone absolute argv tokens as inputs, and skipped private ancestry inside
opaque runtime trees. It also allocated every directory container on HOME's filesystem. These
choices lost relative and joined-option inputs, exposed unmapped foreign runtime ancestry and
changed `/tmp` device/capacity when HOME used another volume. A lexical parent component could
become a bind target and replace already prepared ancestry with original host metadata.

## Considered options

- Trust overflow ownership, weaken AF admission, change measured argv or fall back to an
  unisolated process: rejected. None preserves measurement or original-session authority.
- Enumerate ambient siblings or infer a generic command's option grammar: rejected. Launch
  authority and argument size must depend on declared resources rather than unrelated entries.
- Normalize parent components before following symlinks: rejected. `link/..` has filesystem
  semantics that lexical normalization can change.
- Put every private container on HOME or a fresh tmpfs: rejected. Device, capacity and actual
  free-space admission must remain those of the original relevant volume.
- Declare paths at parsed calling boundaries and construct canonical mount branches: selected.

## Decision

Measurement scripts supply their parsed AF binary, corpus, source inputs, work directories and
report destinations independently of unchanged argv. The trusted local executor uses its existing
readable/writable/capture metadata. Release smoke declares its scratch directory. Real required
inputs fail if missing or inaccessible; optional not-yet-created configuration retains its existing
ancestry. Directory declarations never inspect a fabricated filename. Report files are prepared
without truncation and bound individually when their parent must be reconstructed, so supported
in-place output remains persistent without restoring the parent's unsafe metadata.

Resolve each path component against the actual filesystem, following symlinks before parent
traversal. Retain named symlinks, canonical target branches and necessary traversal directories.
Emit no dot/parent mount target or alias bind that can restore a prepared ancestor. Render parents
before children; reconstructed required ancestry takes precedence over opaque runtime mounts at
any intersection. Fixed runtime tool/library subtrees remain declared inputs, and executable
leaves retain their actual inodes, bytes, modes and ownership.

Allocate each reconstructed container only on its original filesystem device. Prefer private
staging on that device, otherwise use a known writable directory on that same device. Refuse
when safe preparation is impossible. A launch context owns account files and all private backing
directories until process teardown, then removes them, including preparation/launch failures.
No service, persistent helper registry, host parent mutation or new privilege is introduced.
Namespace setup surrounds the outer script; measured AF argv, wait4 counters and timer boundaries
remain unchanged. Contained capability/no-new-privileges handling and production owner checks
remain unchanged.

The accepted research Document
`sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`, from source
Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`,
recommends shared durable subscriptions/outbox and conservative uncertain-admission handling.
This launcher correction preserves that recommendation and the cumulative wake implementation;
it grants no execution/publication authority or live host certification.

## Consequences

Required deterministic tests retain ordinary, search-only and 6000-sibling launches, private NSS,
external-account open tripwires and database/key/WAL/SHM equality. Actual non-root AF inventory,
ready scripted Task planning and exact zero-Attempt missing-Provider smoke receipts also run under
`/var/tmp` and `/usr/local`. A separate temporary volume exercises actual child stat/statvfs and
AF storage reporting. Actual scripts exercise joined/relative AF options, relative corpus input,
report persistence and dynamically created resources through the real local executor. Synthetic
inaccessible/disappearing `entry` probes and parent-relative PATH/symlink cases are private fixtures.
These checks establish implementation behavior, not performance results or Pi/Codex live support;
Claude live remains deferred. No persisted kernel contract or compatibility reader changes.
