# ADR-0113: GA reads only what GA writes

**Status:** accepted (2026-09-21). Supersedes in part the ADRs listed under *Superseded clauses*
and ADR-0015, ADR-0016, ADR-0022, ADR-0023, ADR-0024, ADR-0043, ADR-0051 and ADR-0080 in full.

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
   `af_version` key, `af.toml`'s `[worker.*]` tables and `defaults.task_pipeline` key, and untyped
   pipeline ports, are refused rather than upgraded on read. The retired `.review/` layout is not
   read at all, and `af onboard --migrate` is gone. The fixed implementation v1 format
   (`.af/pipelines/implement.toml` with its `.af/workers/` implementer and evaluator) is not read
   either: `af task start` takes only a Task file, and its `--kind`, `--goal` and `--pipeline`
   flags are gone. A Task catalog Worker declares a `command` or `model` runner; the
   `legacy_task_command` runner, which spoke the fixed format's Worker protocol, is refused.
   Task Review has one generation: a catalog's `review.generation` may be omitted or `2`, and
   either way captures `af.review-task-policy/2`. A reviewer package that lacks the
   `af/TaskReviewAssignment@1` input or declares `af/TaskReviewSubject@1` or
   `review.kernel/ReviewerResult@1` ports is refused at planning. Campaign review likewise has one
   reviewer contract: every reviewer answers a typed `ReviewerResult@2` output, and every reviewer
   and Scatter receives Generation's exact `FindingSet@1`; `PriorFindings@1`, `ReviewerResult@1`
   and untyped reviewer outputs are refused when the pipeline loads.
3. **One contract per name, at its highest pre-GA version.** Each pre-GA version ladder of a
   persisted or emitted contract (event and artifact types, execution and usage records, CLI JSON
   outputs and their JSON Schemas, and catalog and review-policy generations) collapses at GA to a
   single contract that keeps its highest version number. Lower versions are neither written nor
   read. Authored configuration formats keep every version GA still accepts: review pipeline
   formats 2 to 5 stay at GA, and only format 1 and the untyped port shorthand go. Until GA ships,
   a collapsed contract may also change in place under its number instead of taking a new
   version: it may drop a field nothing reads, require one every writer sets, or change what an
   omitted value means. `ReviewerResult@2`, for example, no longer has the reviewer `verdict` and
   `summary` that no decision, projection or display read. This replaces ADR-0002's bump rule, and ADR-0021's new-version rule for
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
  readers, and `RunReport@3` itself; its `authority_unavailable` reason is part of `RunReport@6`.
- [ADR-0021](0021-keep-reviewer-result-wire-shape-flat.md): the permanence of `ReviewerResult@1`,
  and the new-version rule for pre-GA reviewer-result shapes. Its flat report shape and single
  validator carry over to `ReviewerResult@2`.
- [ADR-0025](0025-require-typed-generation-outputs-in-version-2.md): the untyped port shorthand
  kept readable for older pipeline files, and `PriorFindings@1` as a Generation output.
- [ADR-0034](0034-surface-partial-results-without-ledger-authority.md): recorded, not gathered
  results in `af review report`. `af review run` still lists them, from the Round's selected Task
  Attempts.
- [ADR-0035](0035-address-campaign-state-by-opaque-id.md): the permanent compatibility path for
  label-named Campaign directories.
- [ADR-0036](0036-resolve-gate-caches-through-machine-local-bounded-policy.md): the permanent
  `RunReport@1`–`@4` readers, the `RunReport@5` event, whose Cache Snapshot receipts
  `RunReport@6` records as its `cached` execution, and the permanence of pipeline format 1.
- [ADR-0037](0037-default-campaigns-to-one-round-light-review.md): the explicit `--light` flag;
  light review stays the default.
- [ADR-0038](0038-transport-proposals-beside-reviewer-results.md): `ReviewerResult@1` beside
  `ReviewerResult@2`.
- [ADR-0039](0039-own-dynamic-shards-inside-a-typed-scatter-node.md): pipeline format 1 among the
  static formats that keep their frozen semantics.
- [ADR-0041](0041-make-review-selectors-explicit-and-refuse-empty-diffs.md): the compatibility
  spelling `--authority REV`.
- [ADR-0042](0042-require-provider-bindings-and-isolate-claude-reviewers.md): the durable, fenced,
  charged Provider Operation that `af provider doctor` and `af review run` shared, with the
  Campaign/Round admission evidence and smoke budget it left. Both commands now admit Providers
  through the Review Task's captured Provider admission Attempts.
- [ADR-0044](0044-af-manages-itself-and-dispatches-to-the-pinned-release.md): the `afactory/`
  configuration read-through and the `AFACTORY_*` rename refusals.
- [ADR-0045](0045-one-release-train-and-a-pin-that-binds-bytes.md): the 0.7.1 `af_version` lock
  shape, unsigned pre-0.8.0 releases, the 0.7.1 floor, the `.review/` to `.af/` conversion
  (`af onboard --migrate`), replay of `.review/` Campaigns, and the release gate that plans a
  downstream consumer's pinned policy with every built binary.
- [ADR-0046](0046-add-versioned-task-contracts-with-exact-plan-approval.md): retained
  `tasks.sqlite` histories and the idempotent linking of historical Stores.
- [ADR-0048](0048-compile-task-ports-and-fence-developer-plan-decisions.md): the legacy-link
  obligation, and the scheduler's own Gate suppression, which only the pre-Task Review executor
  used. A Review Gate is a Task condition, so a node it blocks is suppressed as an unselected
  branch, and no report records a `gate_blocked` suppression.
- [ADR-0049](0049-run-task-workers-through-shared-durable-attempts.md): retained legacy
  implementation Stores, the common Store's read-only links to legacy histories, the historical
  delivery fixture, and the fixed command implementation entry point with its ADR-0051 migration.
- [ADR-0057](0057-export-portable-task-definitions-without-execution-authority.md): readability of
  previously captured locks.
- [ADR-0062](0062-refresh-issue-revisions-without-resetting-execution-authority.md): the original
  ownership scheme and recovery kept for historical delivery receipts without a result field.
- [ADR-0063](0063-require-goal-acceptance-alongside-embedded-review.md): persisted legacy
  contracts, the frozen v0.8.0 fixtures and the compatibility gate.
- [ADR-0065](0065-persist-task-run-diagnostics-and-recover-domain-publication.md): readability of
  older Tasks without run reports.
- [ADR-0066](0066-reserve-task-attempts-before-binding-exact-context.md): the historical combined
  `Prepared` record and its Store API.
- [ADR-0067](0067-project-common-task-selections-into-canonical-review.md): the legacy Store's
  selection from `AttemptAdmitted@1`.
- [ADR-0068](0068-retain-inflight-task-usage-in-the-common-budget.md): usage observations and
  settlements encoded as `TaskExecutionRecord@1` with numeric charges.
- [ADR-0069](0069-compile-captured-review-ports-with-explicit-artifact-codecs.md): the
  compatibility types that pipeline format 1 shorthand Generation outputs received by port name,
  and the types opaque shorthand Gate, Gather and Ledger ports received by node kind.
- [ADR-0070](0070-separate-review-domain-operations-and-fence-task-dispatch-by-round.md): the
  legacy Kernel that composed `ReviewDomainState` with its own execution and replay owner.
- [ADR-0071](0071-share-captured-review-authority-and-task-token-scopes.md): readability of
  historical `.review/` captures.
- [ADR-0075](0075-retain-exact-task-usage-with-versioned-decimal-counters.md): readers for
  unversioned and numeric-only usage, migration on write, and the execution-record, usage and
  inspection version ladders. Every Task inspection is `af/task-inspection@11`, whose sections
  are each present only when the Task recorded them.
- [ADR-0077](0077-run-captured-review-operations-under-common-task-attempts.md): historical
  name-only Reviewer outputs meaning `ReviewerResult@1`, and the Store's opaque exception for them.
- [ADR-0078](0078-bind-review-conclusions-to-exact-task-accounting.md): readers for historical
  `RunReport` versions and raw provenance.
- [ADR-0079](0079-retain-exact-cumulative-charge-within-one-task-attempt.md): readers for frozen
  execution `@1` and `@2`, usage `@1`, and `af/review-report@1` and `@2`, and the historical
  `AttemptLedger` entry points with replacement semantics; every Attempt charge is an exact
  cumulative floor. Its Broker operations, with `BrokerOperationReceipt@2`, are gone with the
  Broker; the exact u128 cumulative charge they motivated stays.
- [ADR-0081](0081-register-owned-review-children-in-the-common-task-runtime.md): earlier execution
  and inspection generations, and the Review Task policy generations before
  `LegacyReviewTaskPolicy@4` that compiled without owned children.
- [ADR-0082](0082-continue-captured-review-rounds-within-the-original-task.md): earlier
  `TaskTransition` generations and inspection@6.
- [ADR-0083](0083-run-post-round-integration-within-the-original-task.md): earlier inspection
  generations.
- [ADR-0084](0084-route-new-review-commands-through-the-common-task.md): the original executor for
  historical paid Campaigns, and historical readers and output generations.
- [ADR-0085](0085-retain-exact-native-task-usage-across-multiple-turns.md): the frozen Kernel's
  separate `AttemptEvidence`; the exact Task Attempt evidence now carries that name.
- [ADR-0086](0086-record-expired-review-publication-without-restarting-work.md): earlier transition
  and inspection generations.
- [ADR-0087](0087-control-native-task-invocations-through-the-shared-supervisor.md): the Worker
  entry points without a control, and the forwarding that kept their previous behavior. Every
  native Task call is controlled; an adapter honors or refuses each supplied control, and a model
  Worker receives no sandbox-local environment.
- [ADR-0089](0089-interrupt-task-work-when-its-writer-heartbeat-fails.md): the Broker
  accounting, calls and late receipts that the runtime kept outside Worker invocation.
- [ADR-0091](0091-capture-explicit-task-provider-admission-costs.md): the V1 catalog and
  run-authority generation beside V2.
- [ADR-0094](0094-bind-task-review-assignments-and-readable-inputs.md): Review catalog and policy
  generation one as compatibility formats.
- [ADR-0095](0095-bind-legacy-task-context-and-retry-output-admission.md): the *Explicit legacy
  context* section: the `legacy_task_command` runner and its `legacy_budget_tokens` wire budget,
  `af/TaskContext@2` with its compatibility contracts and 8 MiB metadata limit, and the original
  context rendering for previously captured packages, together with the first two paragraphs of
  *Alternatives and consequences* that argue for that design. Its output-admission retry decision
  and the rationale for it stay.
- [ADR-0099](0099-select-task-review-generation-independently-of-provider-costs.md): the semantics
  of captured policy-one plans, and omission selecting policy one. An omitted `review.generation`
  now selects generation two, the only generation.
- [ADR-0101](0101-reuse-review-structure-with-fresh-task-boundaries.md): readability of historical
  duplicate or non-approved revocations, and of unauthenticated revocations without a retained
  proof.
- [ADR-0102](0102-account-for-every-reported-claude-task-model.md): the default-model
  compatibility of a Claude Task adapter invoked without an explicit model restriction. The
  adapter now refuses a command without `--model`, which every Task binding passes. The legacy
  Review envelope and the old provider-smoke accounting that it left as follow-up work are gone
  with the pre-Task Review executor and the Provider Operation.
- [ADR-0104](0104-preview-captured-task-plans-before-first-execution.md): the preview for the
  legacy goal entry point; `task start` takes only a Task file.
- [ADR-0106](0106-authorize-experimental-children-separately.md): earlier inspection and execution
  generations.
- [ADR-0107](0107-carry-worker-notes-and-head-deltas-as-declared-warm-layers.md): the legacy path
  that recorded the Warm Set before the node's first `AttemptDispatched@1`.
- [ADR-0108](0108-carry-gate-build-caches-as-explicitly-unsafe-warm-layers.md): the legacy
  Kernel's in-memory record of a Worker's clone measurement.
- [ADR-0110](0110-capture-sessions-in-two-phases-and-confirm-clean-rounds-cold.md): the Kernel
  reviewer path that ran the session protocol and dispatched Cold Closeout. The protocol, the
  selection gates and the Ledger fold stay as the base for running them on the Task host; until
  then Task-hosted Attempts record `host_unsupported` and no confirmation is dispatched. The
  `AttemptDispatched@1`, `AttemptAdmitted@1` and `AttemptFailed@1` records a confirmation wrote
  are no longer event types; on the Task host a confirmation is a Task Attempt.

ADR-0015 and ADR-0080 are superseded in full. No release shipped a Broker connector and af never
installed one, so no Attempt ever received a Broker Handle: every model adapter reports
`trusted_unsafe`, and a `brokered` reviewer could only be refused. GA ships without a Broker, so
both records are deleted together with it: the handle lease, operation policies and receipts, the
Task log's `TaskBrokerTransition@1` with the `af/TaskBrokerBinding@1` and
`af/TaskBrokerOperation@1` records it named, and `af/task-inspection@4`, the inspection output
that showed them. Two of ADR-0015's decisions stay in force here:

- Pipeline formats 4 and 5 require every reviewer to declare `credential_free` or
  `trusted_unsafe`; formats 2 and 3 make no credential claim. A reviewer whose captured credential
  mode differs from the one its adapter reports is refused before dispatch.
- A runner that can read reusable credentials is `trusted_unsafe` and cannot authorize
  `auto_apply`. The Codex and Claude CLI adapters are `trusted_unsafe`, and redacting their output
  does not upgrade them.

ADR-0016 is superseded in full. Its Provider Operation admitted a Provider before the pre-Task
Review executor dispatched a reviewer: a Round-bound, epoch-fenced structural probe and inference
smoke, recorded as `ProviderOperationTransition@1`, charged to the Campaign budget and continued
with `--resume-provider`. GA runs every Review on the common Task runtime, which admits each
Provider through the Task's own captured probe Attempt
([ADR-0091](0091-capture-explicit-task-provider-admission-costs.md)), so that record is deleted
together with the executor, the flag and `af/provider-doctor@1`. Provider selection keeps its
decision: repeatable `--provider NODE=PROVIDER_ID` bindings resolve against the machine-local
registry and appear in neither the pipeline nor the Campaign Manifest.

ADR-0022 and ADR-0023 are superseded in full. They made a pre-Task reviewer retry's refusal
history durable in the Campaign log: `AttemptInput@1` named the history a retry consumed and was
appended with its `AttemptDispatched@1`, and `AttemptFeedback@1` named the feedback a failed or
fenced Attempt produced, beside its terminal diagnostic. GA writes neither event, nor any other
pre-Task Attempt, Broker binding or receipt event, so both records are deleted together with those
types. Their rule holds for every Task Attempt: retry input is durable authority fixed before the
Attempt runs, as the admitted `feedback_ids` its reservation names
([ADR-0066](0066-reserve-task-attempts-before-binding-exact-context.md)), and produced feedback is
typed and kept apart from diagnostic prose, which is never retry input
([ADR-0095](0095-bind-legacy-task-context-and-retry-output-admission.md)).

ADR-0024 is superseded in full. It gave source Manifests a `path_encoding` generation so that
Manifests written before `percent_v2` stayed readable and an unchanged tree kept its Snapshot
digest. GA reads no such Manifest, so a Manifest has one path spelling, the former `percent_v2`
spelling of `review_core::encode_path`, and carries no generation marker. Snapshot content identity
hashes the stored spelling. Ordinary trees keep their Manifest bytes and digests; a tree with a
path that starts or ends with whitespace, or holds a space together with a `%` or non-UTF-8 bytes,
gets a different digest than a pre-GA release gave it. A file created during a run, which a seal,
warm workspace scan or Task delivery spelled in the old alphabet when the baseline was an ordinary
tree, gets the spelling capture gives it.

ADR-0043 is superseded in full. `.review/` authority is neither migrated nor replayed, so that
record is deleted together with `af onboard --migrate`.

ADR-0051 is superseded in full. It compiled `af task start --goal` and the fixed implementation
v1 format into the common Task runtime; GA runs only Task files, so that record is deleted
together with the adapter. Three of its decisions describe every Task and stay in force here:

- Delivery recovery closes a rolled-back prepared operation with a failed receipt before it
  prepares the retry. Recovery thereby passes the common journal's transition checks and keeps
  the local rollback and operator-content protections.
- A code-task policy's `check_process_wall_ms` bounds each Check process within the aggregate
  Check Attempt, so one slow Check cannot borrow another's allowance.
- A native Task transport timeout or CAS failure refuses the business result but keeps the
  reported usage, and arithmetic overflow saturates the reported counter rather than discarding
  the overrun or panicking. Raw stdout and stderr stay available when CAS storage succeeds.

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
