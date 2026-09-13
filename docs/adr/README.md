# Review Kernel — architecture decisions

Decisions about the kernel's own design. These are distinct from
[`../../../../docs/adr/`](../../../../docs/adr/), which is a generated hub's own decision log —
the kernel ships *into* such a hub, so its decisions travel with the code rather than with the
project using it.

Same rules as the hub's: one decision per file, numbered, **immutable once accepted**. A changed
decision becomes a new ADR marked *superseded by* the old one, with links both ways. Record the
options you rejected and why — that is the part future-you needs.

## Index

- [0001 — Compute the Change Set with `git diff`, reachable only through a typed
  method](0001-tree-diff-behind-a-typed-method.md)
- [0002 — A payload shape change bumps the event type
  version](0002-event-payload-changes-bump-the-type-version.md)
- [0003 — Gate checks may reach host caches through a root-allowlisted
  passthrough](0003-gate-caches-pass-through-to-the-host.md)
- [0004 — Reviewers author patch proposals; the kernel verifies, git
  applies](0004-reviewers-author-verified-patch-proposals.md)
- [0005 — Report artifacts are authoritative for finding
  projections](0005-report-artifacts-are-projection-authority.md)
- [0006 — Finding identity is independent of path and
  title](0006-finding-identity-is-path-independent.md)
- [0007 — Demands are independent blocking
  obligations](0007-demands-are-independent-blocking-obligations.md)
- [0008 — Safe caches are sandbox-local
  snapshots](0008-safe-caches-are-sandbox-local-snapshots.md)
- [0009 — Campaign authority is resolved before candidate
  capture](0009-campaign-authority-is-base-pinned.md)
- [0010 — Proposals are exported by ID and remain bound to their base
  Snapshot](0010-proposals-are-exported-by-id-and-base-bound.md)
- [0011 — Silence is not a Drop](0011-silence-is-not-a-drop.md)
- [0012 — Fixed requires current-Subject
  verification](0012-fixed-requires-current-subject-verification.md)
- [0013 — Scope is evaluated per active Report
  claim](0013-scope-is-evaluated-per-active-claim.md)
- [0014 — Non-fixed resolutions are scoped and
  challengeable](0014-non-fixed-resolutions-are-challengeable.md)
- [0015 — Safe attempts receive handles, not reusable
  secrets](0015-safe-attempts-receive-handles-not-secrets.md)
- [0016 — Provider preflight is a fenced, charged
  operation](0016-provider-preflight-is-a-fenced-operation.md)
- [0017 — Record rename truncation and continue the diff
  Subject](0017-record-rename-truncation-and-continue.md)
- [0018 — Share one bounded infrastructure
  executor](0018-share-one-bounded-infrastructure-executor.md)
- [0019 — Report authority failures
  explicitly](0019-report-authority-failures-explicitly.md)
- [0020 — Stream CAS materialization and clone duplicate
  files](0020-stream-cas-materialization-and-clone-duplicates.md)
- [0021 — Keep the ReviewerResult wire shape
  flat](0021-keep-reviewer-result-wire-shape-flat.md)
- [0022 — Persist retry feedback as an Attempt
  input](0022-persist-retry-feedback-as-attempt-input.md)
- [0023 — Separate retry feedback from terminal
  diagnostics](0023-separate-retry-feedback-from-terminal-diagnostics.md)
- [0024 — Version manifest path encoding without changing Snapshot content
  identity](0024-version-manifest-path-encoding.md)
- [0025 — Require typed Generation outputs in pipeline version
  2](0025-require-typed-generation-outputs-in-version-2.md)
- [0026 — Share process supervision through a leaf
  crate](0026-share-process-supervision-through-a-leaf-crate.md)
- [0027 — Use one correctness reviewer per
  milestone](0027-use-one-correctness-reviewer-per-milestone.md)
- [0028 — Prioritize wise token use and minimum Worker
  context](0028-prioritize-wise-token-use-and-minimum-worker-context.md)
- [0029 — Dogfood the candidate `af` before v1 is
  complete](0029-dogfood-the-candidate-af-before-v1-is-complete.md)
- [0030 — Complete minimal v1 and v2 before candidate
  dogfood](0030-complete-minimal-v1-and-v2-before-dogfood.md)
- [0031 — Deliver verified Tasks only to new local
  worktrees](0031-deliver-verified-tasks-to-new-local-worktrees.md)
- [0032 — Generate review authority with
  `af onboard`](0032-generate-review-authority-with-af-onboard.md)
- [0033 — Treat configured Workers as authorization for declared input
  delivery](0033-configured-workers-authorize-declared-input-delivery.md)
- [0034 — Surface partial results without granting Ledger
  authority](0034-surface-partial-results-without-ledger-authority.md)
- [0035 — Address Campaign state by opaque
  ID](0035-address-campaign-state-by-opaque-id.md)
- [0036 — Resolve Gate caches through machine-local bounded
  policy](0036-resolve-gate-caches-through-machine-local-bounded-policy.md)
- [0037 — Default Campaigns to one-Round light
  review](0037-default-campaigns-to-one-round-light-review.md)
- [0038 — Transport Proposal declarations beside Reviewer
  Results](0038-transport-proposals-beside-reviewer-results.md)
- [0039 — Own dynamic shards inside a typed Scatter
  node](0039-own-dynamic-shards-inside-a-typed-scatter-node.md)
- [0040 — Promote only checked derived
  Snapshots](0040-promote-only-checked-derived-snapshots.md)
- [0041 — Make review selectors explicit and refuse empty
  Diffs](0041-make-review-selectors-explicit-and-refuse-empty-diffs.md)
- [0042 — Require Provider bindings and isolate Claude
  reviewers](0042-require-provider-bindings-and-isolate-claude-reviewers.md)
- [0043 — Drop legacy `.review/` authority in
  v0.8.0](0043-drop-legacy-review-authority-in-v0-8-0.md)
- [0044 — `af` manages itself: dispatch to the pinned release, policy-driven updates, a
  layered configuration](0044-af-manages-itself-and-dispatches-to-the-pinned-release.md)
- [0045 — One release train, and a pin that binds
  bytes](0045-one-release-train-and-a-pin-that-binds-bytes.md)
- [0046 — Add versioned Task contracts with exact-plan
  approval](0046-add-versioned-task-contracts-with-exact-plan-approval.md)
- [0047 — Preserve Task wire identity and review
  completeness](0047-preserve-task-wire-identity-and-review-completeness.md)
- [0048 — Compile Task ports and fence developer plan
  decisions](0048-compile-task-ports-and-fence-developer-plan-decisions.md)
- [0049 — Run Task Workers through shared durable
  Attempts](0049-run-task-workers-through-shared-durable-attempts.md)
- [0050 — Reduce Review Tasks with the canonical domain
  Ledger](0050-reduce-review-tasks-with-the-canonical-domain-ledger.md)
- [0051 — Compile fixed implementation Tasks into the common
  runtime](0051-compile-fixed-implementation-tasks-into-the-common-runtime.md)
- [0052 — Capture local bindings and compose Review
  acceptance](0052-capture-local-bindings-and-compose-review-acceptance.md)
- [0053 — Resolve shared catalogs only during explicit
  sync](0053-resolve-shared-catalogs-only-during-explicit-sync.md)

- [0054 — Keep targeted repair distinct from complete Review](0054-keep-targeted-repair-distinct-from-complete-review.md)
- [0055 — Select captured Pipelines before generation](0055-select-captured-pipelines-before-generation.md)
- [0056 — Share planning accounting and authenticate generated plan decisions](0056-share-planning-accounting-and-authenticate-generated-plan-decisions.md)
- [0057 — Export portable Task definitions without execution authority](0057-export-portable-task-definitions-without-execution-authority.md)
- [0058 — Verify document artifacts through the common Task runtime](0058-verify-document-artifacts-through-the-common-task-runtime.md)
- [0059 — Carry Task fix evidence into bounded heavy Review](0059-carry-task-fix-evidence-into-bounded-heavy-review.md)
- [0060 — Generate working starters from supported contracts](0060-generate-working-starters-from-supported-contracts.md)
- [0061 — Capture read-only issue sources outside execution authority](0061-capture-read-only-issue-sources-outside-execution-authority.md)
- [0062 — Refresh issue revisions without resetting execution authority](0062-refresh-issue-revisions-without-resetting-execution-authority.md)
- [0063 — Require goal acceptance alongside embedded Review](0063-require-goal-acceptance-alongside-embedded-review.md)
- [0064 — Reuse structural validation with fresh authority checks](0064-reuse-structural-validation-with-fresh-authority-checks.md)
- [0065 — Persist Task run diagnostics and recover domain publication](0065-persist-task-run-diagnostics-and-recover-domain-publication.md)
- [0066 — Reserve Task Attempts before binding exact
  context](0066-reserve-task-attempts-before-binding-exact-context.md)
- [0067 — Project common Task selections into canonical
  Review](0067-project-common-task-selections-into-canonical-review.md)
- [0068 — Retain in-flight Task usage in the common
  budget](0068-retain-inflight-task-usage-in-the-common-budget.md)
- [0069 — Compile captured Review ports with explicit artifact
  codecs](0069-compile-captured-review-ports-with-explicit-artifact-codecs.md)
- [0070 — Separate Review domain operations and fence Task dispatch by Round](0070-separate-review-domain-operations-and-fence-task-dispatch-by-round.md)
- [0071 — Share captured Review authority and Task token scopes](0071-share-captured-review-authority-and-task-token-scopes.md)
- [0072 — Retain process output independently of transport status](0072-retain-process-output-independently-of-transport-status.md)
- [0073 — Check Task retry eligibility before reservation](0073-check-task-retry-eligibility-before-reservation.md)
- [0074 — Isolate concurrent process pipe creation on Apple](0074-isolate-concurrent-process-pipe-creation-on-apple.md)
- [0075 — Retain exact Task usage with versioned decimal counters](0075-retain-exact-task-usage-with-versioned-decimal-counters.md)
- [0076 — Decode and verify typed CAS reads
  once](0076-decode-and-verify-typed-cas-reads-once.md)
- [0077 — Run captured Review operations under common Task
  Attempts](0077-run-captured-review-operations-under-common-task-attempts.md)
- [0078 — Bind Review conclusions to exact Task accounting](0078-bind-review-conclusions-to-exact-task-accounting.md)
- [0079 — Retain exact cumulative charge within one Task Attempt](0079-retain-exact-cumulative-charge-within-one-task-attempt.md)
- [0080 — Bind Broker evidence to the original Task Attempt](0080-bind-broker-evidence-to-the-original-task-attempt.md)
- [0081 — Register owned Review children in the common Task runtime](0081-register-owned-review-children-in-the-common-task-runtime.md)
- [0082 — Continue captured Review Rounds within the original Task](0082-continue-captured-review-rounds-within-the-original-task.md)
- [0083 — Run post-Round Integration within the original Task](0083-run-post-round-integration-within-the-original-task.md)
- [0084 — Route new Review commands through the common Task](0084-route-new-review-commands-through-the-common-task.md)
- [0085 — Retain exact native Task usage across multiple turns](0085-retain-exact-native-task-usage-across-multiple-turns.md)
- [0086 — Record expired Review publication without restarting work](0086-record-expired-review-publication-without-restarting-work.md)
- [0087 — Control native Task invocations through the shared supervisor](0087-control-native-task-invocations-through-the-shared-supervisor.md)
- [0088 — Retain native billing completeness with Task usage](0088-retain-native-billing-completeness-with-task-usage.md)
- [0089 — Interrupt Task work when its writer heartbeat fails](0089-interrupt-task-work-when-its-writer-heartbeat-fails.md)
- [0090 — Recheck native Task Provider identity before private invocation](0090-recheck-native-task-provider-identity-before-private-invocation.md)
- [0091 — Capture explicit Task Provider admission costs](0091-capture-explicit-task-provider-admission-costs.md)
- [0092 — Capture common Review admission reservations](0092-capture-common-review-admission-reservations.md)
- [0093 — Derive CodeTask acceptance from execution and evidence](0093-derive-code-task-acceptance-from-execution-and-evidence.md)
- [0095 — Bind legacy Task context and retry output admission](0095-bind-legacy-task-context-and-retry-output-admission.md)
- [0096 — Revalidate Task execution evidence on cached
  replay](0096-revalidate-task-execution-evidence-on-cached-replay.md)
- [0097 — Share validated source reads within one
  operation](0097-share-validated-source-reads-within-one-operation.md)
