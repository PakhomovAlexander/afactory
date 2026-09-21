# ADR-0113: GA reads only what GA writes

**Status:** accepted (2026-09-21). Supersedes in part the ADRs listed under *Superseded clauses*
and ADR-0043 in full.

Before GA, Afactory treated everything it had ever written as a permanent obligation.
[ADR-0002](0002-event-payload-changes-bump-the-type-version.md) made every superseded event reader
permanent, and later decisions extended that promise to Report artifacts, Finding fingerprints, run
reports, Campaign directories, Task execution records, usage encodings, inspection outputs,
captured Task context, the `.review/` authority layout, lock shapes and pre-0.8.0 releases. Each
promise kept alive a reader, fallback, migration or second executor that the current release never
uses on data it writes, along with the tests and fixtures that replay those bytes. By 0.9.0-rc.6
`af/task-inspection` alone had eleven versions, `@1` through `@11`, and a second Review executor
remained only for Campaigns started before the common Task runtime.

Pre-GA users are few, and they can finish or discard their in-flight work before upgrading. The
GA release sets the baseline users will hold Afactory to, so it should carry only what it needs to
read its own output.

## Decision

GA reads only what GA writes. Compatibility obligations start at the GA release.

1. **Pre-GA state is unsupported.** Review Campaigns and Tasks written by a 0.x release, with
   their event logs, CAS objects, sidecars, leases and captured authority, under
   `$XDG_STATE_HOME/af/review/`, `$XDG_STATE_HOME/af/task/` or a directory passed with `--state`
   or `--state-root`, are unsupported. GA makes no promise to read, migrate or refuse such state.
   Where GA recognizes it cheaply, such as a Campaign that predates the common Task runtime, it
   refuses with a message that names the upgrade step; such a check matches only records GA never
   writes, so it never refuses GA's own state. Otherwise pre-GA state may fail on an unknown type
   or shape, or be read as though GA wrote it. Users finish or abandon it under the release that
   started it, then delete it. Machine-local configuration and `af self`'s installed versions and
   activation history keep their current formats and are unaffected.
2. **Committed authority uses the shapes GA accepts.** `.af/` project files, locks, pipelines,
   catalogs and Worker packages are read only in the shapes GA accepts. Keys and shorthands that
   only earlier releases wrote or accepted, such as the lock's `[reviewers]` table and 0.7.1
   `af_version` key, `af.toml`'s `[worker.*]` tables and untyped pipeline ports, are refused rather
   than upgraded on read. The retired `.review/` layout is not read at all, and
   `af onboard --migrate` is gone.
3. **One contract per name, at its highest pre-GA version.** Each pre-GA version ladder of a
   persisted or emitted contract (event and artifact types, execution and usage records, CLI JSON
   outputs and their JSON Schemas, and catalog and review-policy generations) collapses at GA to a
   single contract that keeps its highest version number. Lower versions are neither written nor
   read. Authored configuration formats keep every version GA still accepts: review pipeline
   formats 2 to 5 stay at GA, and only format 1 and the untyped port shorthand go. Until GA ships,
   a collapsed contract may also change in place under its number instead of taking a new
   version: it may drop a field nothing reads, require one every writer sets, or change what an
   omitted value means. This replaces ADR-0002's bump rule, and ADR-0021's new-version rule for
   reviewer results, for pre-GA contracts only. From GA on, both rules apply unchanged to every
   contract GA ships.
4. **Persisted names keep their spelling.** This decision retires readers, not names.
   `review.kernel/*` and `af/*` type strings, schema IDs and established Review Kernel terms stay
   as they are, and the AGENTS.md rule against renaming them stands: a rename would change content
   IDs and every digest-bearing fixture. Rust identifiers, module names and internal wording may
   be renamed.
5. **Self-management stays cross-release, from 0.8.0.** Dispatch runs the pinned release's own
   binary against the state that release wrote, so pins and `af self` still span releases. The
   floor becomes 0.8.0, the first release with signed checksums: nothing older is installed,
   activated or dispatched to, and a binary that embeds the release key refuses every release whose
   `SHA256SUMS` signature is missing or does not verify. The pre-rename `afactory/` configuration
   directory is not read, and `AFACTORY_*` variables have no meaning.
6. **Superseded ADRs.** An accepted ADR's body stays immutable. A partially superseded ADR keeps its
   body and gains a status-line note that links the ADR superseding it. A fully superseded ADR is
   deleted together with its index entry, and git history keeps it. Links to a deleted ADR are
   rewritten to point at the superseding ADR, or to plain text; this is the only edit allowed in
   another accepted ADR's body.

## Superseded clauses

Each ADR below keeps its body and carries a status-line note; only the named clauses lose force.

- [ADR-0002](0002-event-payload-changes-bump-the-type-version.md): the permanent `@1` reader arm,
  and the version bump for pre-GA contracts.
- [ADR-0005](0005-report-artifacts-are-projection-authority.md): the permanent reader of the M1
  flat Report artifact, and the frozen `FindingReported@1` fallback for artifact-less legacy
  imports.
- [ADR-0006](0006-finding-identity-is-path-independent.md): the legacy `file + normalized title`
  fingerprint kept for replaying existing Campaigns.
- [ADR-0011](0011-silence-is-not-a-drop.md): the `RunReport@2` it names; GA writes only
  `RunReport@6`.
- [ADR-0019](0019-report-authority-failures-explicitly.md): the permanent `RunReport@1` and `@2`
  readers.
- [ADR-0021](0021-keep-reviewer-result-wire-shape-flat.md): the permanence of `ReviewerResult@1`,
  and the new-version rule for pre-GA reviewer-result shapes.
- [ADR-0022](0022-persist-retry-feedback-as-attempt-input.md): `AttemptInput@1` as a permanent
  event type.
- [ADR-0023](0023-separate-retry-feedback-from-terminal-diagnostics.md): `AttemptFeedback@1` as a
  permanent event type.
- [ADR-0025](0025-require-typed-generation-outputs-in-version-2.md): the untyped port shorthand
  kept readable for older pipeline files.
- [ADR-0035](0035-address-campaign-state-by-opaque-id.md): the permanent compatibility path for
  label-named Campaign directories.
- [ADR-0036](0036-resolve-gate-caches-through-machine-local-bounded-policy.md): the permanent
  `RunReport@1`–`@4` readers and the permanence of pipeline format 1.
- [ADR-0044](0044-af-manages-itself-and-dispatches-to-the-pinned-release.md): the `afactory/`
  configuration read-through and the `AFACTORY_*` rename refusals.
- [ADR-0045](0045-one-release-train-and-a-pin-that-binds-bytes.md): the 0.7.1 `af_version` lock
  shape, unsigned pre-0.8.0 releases, the 0.7.1 floor, replay of `.review/` Campaigns, and the
  release gate that plans a downstream consumer's pinned policy with every built binary.
- [ADR-0051](0051-compile-fixed-implementation-tasks-into-the-common-runtime.md): readers for the
  original implementation history and delivery schemas, and the v0.8.0 compatibility fixture.
- [ADR-0057](0057-export-portable-task-definitions-without-execution-authority.md): readability of
  previously captured locks.
- [ADR-0063](0063-require-goal-acceptance-alongside-embedded-review.md): persisted legacy
  contracts, the frozen v0.8.0 fixtures and the compatibility gate.
- [ADR-0065](0065-persist-task-run-diagnostics-and-recover-domain-publication.md): readability of
  older Tasks without run reports.
- [ADR-0066](0066-reserve-task-attempts-before-binding-exact-context.md): the historical combined
  `Prepared` record.
- [ADR-0071](0071-share-captured-review-authority-and-task-token-scopes.md): readability of
  historical `.review/` captures.
- [ADR-0075](0075-retain-exact-task-usage-with-versioned-decimal-counters.md): readers for
  unversioned and numeric-only usage, migration on write, and the execution-record and usage
  version ladders.
- [ADR-0078](0078-bind-review-conclusions-to-exact-task-accounting.md): readers for historical
  `RunReport` versions and raw provenance.
- [ADR-0079](0079-retain-exact-cumulative-charge-within-one-task-attempt.md): readers for frozen
  execution `@1` and `@2`, usage `@1`, and `af/review-report@1` and `@2`.
- [ADR-0081](0081-register-owned-review-children-in-the-common-task-runtime.md): earlier execution
  and inspection generations.
- [ADR-0082](0082-continue-captured-review-rounds-within-the-original-task.md): earlier
  `TaskTransition` generations.
- [ADR-0083](0083-run-post-round-integration-within-the-original-task.md): earlier inspection
  generations.
- [ADR-0084](0084-route-new-review-commands-through-the-common-task.md): the original executor for
  historical paid Campaigns, and historical readers and output generations.
- [ADR-0086](0086-record-expired-review-publication-without-restarting-work.md): earlier transition
  and inspection generations.
- [ADR-0091](0091-capture-explicit-task-provider-admission-costs.md): the V1 catalog and
  run-authority generation beside V2.
- [ADR-0094](0094-bind-task-review-assignments-and-readable-inputs.md): Review catalog and policy
  generation one as compatibility formats.
- [ADR-0095](0095-bind-legacy-task-context-and-retry-output-admission.md): `af/TaskContext@1` and
  the original context rendering for previously captured packages.
- [ADR-0099](0099-select-task-review-generation-independently-of-provider-costs.md): the semantics
  of captured policy-one plans.
- [ADR-0101](0101-reuse-review-structure-with-fresh-task-boundaries.md): readability of historical
  duplicate or non-approved revocations.
- [ADR-0106](0106-authorize-experimental-children-separately.md): earlier inspection and execution
  generations.

ADR-0043 is superseded in full. `.review/` authority is neither migrated nor replayed, so that
record is deleted together with `af onboard --migrate`.

## Considered options

- **Keep every reader permanently (ADR-0002).** Rejected: GA would ship, and test forever, readers
  for state no GA user holds, and every contract would keep its whole version ladder.
- **Ship a one-time migration from pre-GA state to GA formats.** Rejected: the migrator must itself
  read every pre-GA generation, so the code GA is meant to lose would stay, exercised only by
  fixtures. Discarding pre-GA state costs its few users one rerun.
- **Deprecate for one release.** Rejected: GA sets the baseline. Shipping the old readers in it,
  even with warnings, defers the same cut and makes them part of what GA promises.
- **Renumber every collapsed contract to `@1`.** Rejected: `@1` already names the oldest and most
  different pre-GA shapes, so a stray pre-GA record could parse under a GA name. Keeping the
  highest number also keeps the type strings that current consumers match.
- **Rename `review.kernel/*` to `af/*` at the same time.** Rejected: it changes content IDs and
  every digest-bearing fixture for a cosmetic gain, and no reader needs it.

## Consequences

- Upgrading to GA means finishing or abandoning in-flight Campaigns and Tasks under the release
  that started them, then deleting `$XDG_STATE_HOME/af/review/`, `$XDG_STATE_HOME/af/task/` and
  any explicit state directory. A key in a committed `.af/` file that GA refuses is deleted by
  hand, because `af onboard --refresh-lock` cannot repair a file it cannot parse. The GA release
  notes say both.
- Code whose only purpose is to read, replay, migrate or render pre-GA state, formats or flags, or
  releases before 0.8.0, is deleted, together with the tests and fixtures that replay pre-GA
  bytes. A test that covers behavior the live path shares is ported to the live path before the
  old one goes.
- No command falls back to a reader, default or executor that it would not use for GA-written data.
- After GA, the state, configuration and contracts GA writes are the baseline, and ADR-0002 and
  ADR-0021 govern every later change to them.
