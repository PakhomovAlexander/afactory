# Architecture decisions

The binding decisions behind the Review Kernel and the `af` command. They travel with the code
rather than with any project that uses it: a consuming repository records its own decisions in
its own log, and the kernel's are all here.

One decision per file, numbered in order of acceptance as `NNNN-kebab-case-title.md`, **immutable
once accepted**. Each opens with a status line carrying the status and date, states the context,
lists the considered options with the reasons each was rejected, records the decision, and ends
with its consequences. A changed decision is a new ADR that names what it supersedes. A partially
superseded ADR keeps its body as it was and gains a note on its status line linking the new ADR,
and that status line may be restated when the new ADR spends the transition wording it carried; a
fully superseded ADR is deleted with its index entry, and git history keeps it, unless the
superseding ADR keeps it with a superseded status line, as ADR-0145 keeps ADR-0110. Links to a
deleted ADR, or to an internal record or completed design note that was deleted, are rewritten to
point at the superseding ADR or to plain text; this is the only edit allowed in another accepted
ADR's body ([ADR-0113](0113-ga-reads-only-what-ga-writes.md), clauses 6 and 8;
[ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md), clause 8).

A number is never reissued: a deleted record leaves its number empty, and the index simply skips
it. Two records were accepted on the same day under the number 0107 — the warm-layer ADR and the
release-validation ADR listed beside it. Both keep the number they were accepted under, for
different reasons: the release-validation record keeps its number by owner decision, and the
warm-layer record keeps its because renumbering it would rewrite links inside the accepted
ADR-0108, ADR-0109 and ADR-0110 and in a shipped release section. 0107 is not issued again.

To propose one, take the next number after the highest in the index below, write it in that
shape, add it to the index, and open a pull request; it becomes binding when the pull request
merges with the status `accepted`. Record the options you rejected and why — that is the part
future readers need.

## Index

- [0001 — Compute the Change Set with `git diff`, reachable only through a typed
  method](0001-tree-diff-behind-a-typed-method.md)
- [0002 — A payload shape change bumps the event type
  version](0002-event-payload-changes-bump-the-type-version.md)
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
- [0025 — Require typed Generation outputs in pipeline version
  2](0025-require-typed-generation-outputs-in-version-2.md)
- [0026 — Share process supervision through a leaf
  crate](0026-share-process-supervision-through-a-leaf-crate.md)
- [0027 — Use one correctness reviewer per
  milestone](0027-use-one-correctness-reviewer-per-milestone.md)
- [0028 — Prioritize wise token use and minimum Worker
  context](0028-prioritize-wise-token-use-and-minimum-worker-context.md)
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
- [0044 — `af` manages itself: dispatch to the pinned release, policy-driven updates, a
  layered configuration](0044-af-manages-itself-and-dispatches-to-the-pinned-release.md)
  (superseded in part by [ADR-0154](0154-check-for-updates-after-headless-and-json-commands.md))
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
- [0094 — Bind Task Review assignments and readable
  inputs](0094-bind-task-review-assignments-and-readable-inputs.md)
- [0095 — Bind legacy Task context and retry output admission](0095-bind-legacy-task-context-and-retry-output-admission.md)
- [0096 — Revalidate Task execution evidence on cached
  replay](0096-revalidate-task-execution-evidence-on-cached-replay.md)
- [0097 — Share validated source reads within one
  operation](0097-share-validated-source-reads-within-one-operation.md)
- [0098 — Scope Review memos to one domain operation](0098-scope-review-memos-to-one-domain-operation.md)
- [0099 — Select Task Review generation independently of Provider
  costs](0099-select-task-review-generation-independently-of-provider-costs.md)
- [0100 — Preserve issue hierarchy and selected-field refresh semantics](0100-preserve-issue-hierarchy-and-selected-field-refresh.md)
- [0101 — Reuse Review structure with fresh Task
  boundaries](0101-reuse-review-structure-with-fresh-task-boundaries.md)
- [0102 — Account for every reported Claude Task
  model](0102-account-for-every-reported-claude-task-model.md)
- [0103 — Constrain native Claude Task
  replies](0103-constrain-native-claude-task-replies.md)
- [0104 — Preview captured Task plans before first
  execution](0104-preview-captured-task-plans-before-first-execution.md)
- [0105 — Dispatch `self optimize` through the project
  pin](0105-dispatch-self-optimize-through-the-project-pin.md)
- [0106 — Authorize experimental children separately inside one
  Task](0106-authorize-experimental-children-separately.md)
- [0107 — Carry Worker Notes and Head Deltas as declared warm
  layers](0107-carry-worker-notes-and-head-deltas-as-declared-warm-layers.md)
- [0107 — Share release validation and overlap builds](0107-share-release-validation-and-overlap-builds.md)
- [0108 — Carry Gate build caches as explicitly unsafe warm
  layers](0108-carry-gate-build-caches-as-explicitly-unsafe-warm-layers.md)
- [0109 — Re-base Warm Workspaces at stable roots with digest
  verification](0109-rebase-warm-workspaces-at-stable-roots-with-digest-verification.md)
- [0110 — Capture Claude sessions in two phases and confirm a clean warm Round
  cold](0110-capture-sessions-in-two-phases-and-confirm-clean-rounds-cold.md) (superseded by
  [ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md))
- [0111 — Keep Provider bootstrap machine-local and cross-release
  safe](0111-keep-provider-bootstrap-machine-local-and-cross-release-safe.md)
- [0112 — Refuse agent-mediated Provider logins and separate status from
  usage](0112-refuse-agent-mediated-provider-logins.md)
- [0113 — GA reads only what GA writes](0113-ga-reads-only-what-ga-writes.md) (superseded in part
  by [ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md))
- [0114 — Budget CLI Task fixtures for loaded machines, never for a fast
  one](0114-budget-cli-task-fixtures-for-loaded-machines.md)
- [0115 — Declare the `.af/` layout once and keep Task files out of
  git](0115-declare-the-af-layout-and-keep-task-files-out-of-git.md)
- [0116 — Report undeclared `.af/` paths from the manifest, and let a project refuse
  delivery](0116-report-undeclared-af-paths-and-let-a-project-refuse-them.md)
- [0117 — Bind a Task input port to a recorded Task's
  output](0117-bind-task-inputs-to-recorded-task-outputs.md)
- [0118 — Let a review Worker that declares `execute-checks` run in an ephemeral
  clone](0118-let-review-workers-execute-checks-in-an-ephemeral-clone.md) (amended 2026-10-20:
  [a `setsid` child](0118-let-review-workers-execute-checks-in-an-ephemeral-clone.md#bounds-and-children))
- [0119 — Open a read-first browser on bare `af`](0119-open-a-read-first-browser-on-bare-af.md)
- [0120 — Give a source-writing Worker that declares `execute-checks` a
  shell](0120-give-a-source-writing-worker-a-shell.md) (amended 2026-10-20 with ADR-0118)
- [0121 — Show recorded Tasks in the browser from their inspection
  documents](0121-show-recorded-tasks-in-the-browser.md)
- [0122 — Show committed Workers and the Attempts their recorded plans
  bound](0122-show-committed-workers-and-their-recorded-attempts.md)
- [0123 — Hand the terminal to `af` commands typed in the
  browser](0123-hand-the-terminal-to-af-commands-typed-in-the-browser.md)
- [0124 — Run tests in parallel processes and link them
  once](0124-run-tests-in-parallel-processes-and-link-them-once.md)
- [0125 — Charge a Claude model breakdown that covers the top-level
  summary](0125-charge-a-claude-model-breakdown-that-covers-the-top-level-summary.md)
- [0126 — Keep the captured native executable when its launcher
  moves](0126-keep-the-captured-native-executable-when-its-launcher-moves.md)
- [0127 — Snapshot pinned Rust before native Task checks](0127-snapshot-pinned-rust-before-native-task-checks.md)
- [0128 — Renew a live Task writer's lease through its own Store
  connection](0128-renew-a-live-task-writer-lease-through-its-own-connection.md)
- [0129 — Stop Task Workers when af is
  interrupted](0129-stop-task-workers-when-af-is-interrupted.md)
- [0130 — Report a Provider CLI that cannot start as an installation
  failure](0130-report-a-provider-cli-that-cannot-start.md)
- [0131 — Warm Task checks through a toolchain-keyed, bounded, machine-local
  cache](0131-warm-task-checks-through-a-toolchain-keyed-bounded-cache.md) (amended 2026-10-07 by
  [ADR-0144](0144-hold-afs-disk-use-to-a-machine-budget.md))
- [0132 — Measure and compare source candidates in the
  kernel](0132-measure-and-compare-source-candidates-in-the-kernel.md)
- [0133 — Accept reports bound to an exact source
  Snapshot](0133-accept-reports-bound-to-an-exact-source-snapshot.md)
- [0134 — Bind any declared root port to recorded Task
  outputs](0134-bind-any-declared-root-port-to-recorded-task-outputs.md)
- [0135 — Collect finished Tasks behind a tombstone and a reachability
  sweep](0135-collect-finished-tasks-behind-a-tombstone-and-a-reachability-sweep.md) (amended
  2026-10-07 by [ADR-0144](0144-hold-afs-disk-use-to-a-machine-budget.md))
- [0136 — Remove a registered Provider by ID](0136-remove-a-registered-provider-by-id.md)
- [0137 — Permit Provider logins through private host
  capabilities](0137-permit-provider-logins-through-private-host-capabilities.md)
- [0138 — The repository pins its newest release](0138-the-repository-pins-its-newest-release.md)
- [0139 — Deliver native login challenges to a verified private
  requester](0139-deliver-native-login-challenges-to-a-verified-private-requester.md)
- [0140 — Run a declared check through a gate pull
  request](0140-run-a-declared-check-through-a-gate-pull-request.md) (amended 2026-10-05: [the
  pipeline chooses](0140-run-a-declared-check-through-a-gate-pull-request.md#amendment-2026-10-05-the-pipeline-chooses);
  2026-10-07 by [ADR-0144](0144-hold-afs-disk-use-to-a-machine-budget.md); 2026-10-20: [what
  the workflow runs](0140-run-a-declared-check-through-a-gate-pull-request.md#amendment-2026-10-20-what-the-workflow-runs))
- [0141 — List a logged-out default context only until its kind is
  registered](0141-list-a-logged-out-default-context-only-until-its-kind-is-registered.md)
- [0142 — Carry the `af task report` of its Tasks in every pull
  request](0142-carry-the-af-task-report-in-every-pull-request.md)
- [0143 — Charge zero and record unknown usage when no usage is
  reported](0143-charge-zero-and-record-unknown-usage-when-no-usage-is-reported.md)
- [0144 — Hold af's disk use to a machine budget](0144-hold-afs-disk-use-to-a-machine-budget.md)
- [0145 — Read only what this release writes, and version every changed
  shape](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)
- [0146 — Refuse a Provider CLI that does not accept af's
  invocation](0146-refuse-a-provider-cli-that-does-not-accept-afs-invocation.md)
- [0147 — Stop the Claude model fallback, and name the model that
  answered](0147-stop-the-claude-model-fallback-and-name-the-model-that-answered.md)
- [0148 — Discover each Provider's models, and validate every binding before anything is
  charged](0148-discover-provider-models-and-validate-every-binding-before-a-charge.md)
- [0149 — Never wrap a suggested command inside a shell
  token](0149-never-wrap-a-suggested-command-inside-a-shell-token.md)
- [0150 — Name the path a status-line error
  carries](0150-name-the-path-a-status-line-error-carries.md)
- [0151 — Export a human presentation of a report beside its canonical
  Markdown](0151-export-a-human-presentation-of-a-report-beside-its-canonical-markdown.md)
- [0152 — Push the release pin with a deploy key that bypasses main's
  ruleset](0152-push-the-release-pin-with-a-deploy-key-that-bypasses-mains-ruleset.md)
- [0153 — Record telemetry in its own store, and never let it
  steer](0153-record-telemetry-in-its-own-store-and-never-let-it-steer.md)
- [0154 — Check for updates after headless and `--json`
  commands](0154-check-for-updates-after-headless-and-json-commands.md)
- [0155 — Publish a fixture marker whole or not at
  all](0155-publish-a-fixture-marker-whole-or-not-at-all.md)
- [0156 — Give every `make test` run a private JUnit
  store](0156-give-every-make-test-run-a-private-junit-store.md)
- [0157 — Print a Task package digest with the pin
  recipe](0157-print-a-task-package-digest-with-the-pin-recipe.md)
- [0158 — Give a repository one Project identity and register its checkouts as
  Workspaces](0158-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md)
- [0159 — Measure a Project into Surveys with a token-free
  Surveyor](0159-measure-a-project-into-surveys-with-a-token-free-surveyor.md)
- [0160 — Carry a bounded Project Brief on a declared
  port](0160-carry-a-bounded-project-brief-on-a-declared-port.md)
- [0161 — Refresh native check source mtimes before reusing a Cargo
  target](0161-refresh-native-check-source-mtimes-before-reusing-cargo-target.md)
- [0162 — Freeze performance plans and judge them by exact paired
  verdicts](0162-freeze-performance-plans-and-judge-them-by-exact-paired-verdicts.md)
- [0163 — Measure native workloads through a public-API probe and independent golden
  oracles](0163-measure-native-workloads-through-a-public-api-probe-and-independent-golden-oracles.md)
- [0164 — Judge only declared probe requests against a reference-checked
  golden](0164-judge-only-declared-probe-requests-against-a-reference-checked-golden.md)
- [0165 — Own probe scratch in the driver and take lease expiry only from the
  Store](0165-own-probe-scratch-in-the-driver-and-take-lease-expiry-only-from-the-store.md)
- [0166 — Build frozen plans and supervise samples with one authority
  instrument](0166-build-frozen-plans-and-supervise-samples-with-one-authority-instrument.md)
- [0167 — Bound every supervisor wait and verify only what was
  observed](0167-bound-every-supervisor-wait-and-verify-only-what-was-observed.md)
- [0168 — Run frozen shards with whole-CLI oracles and only a per-sample
  scratch](0168-run-frozen-shards-with-whole-cli-oracles-and-only-a-per-sample-scratch.md)
- [0169 — Coordinate a machine, seed each side once, and build Linux recipes in a pinned
  image](0169-coordinate-a-machine-seed-each-side-once-and-build-linux-recipes-in-a-pinned-image.md)
- [0170 — Seed native fixtures, freeze the build image and bound the run by one
  deadline](0170-seed-native-fixtures-freeze-the-build-image-and-bound-the-run-by-one-deadline.md)
- [0171 — Cancel samples through their supervisor, carry the deadline through preflight, and
  observe history order](0171-cancel-samples-through-their-supervisor-carry-the-deadline-through-preflight-and-observe-history-order.md)
- [0172 — Admit candidate files through bounded descriptors, observe streamed JSON by path, and
  report, archive and trend runs offline](0172-admit-candidate-files-through-bounded-descriptors-and-report-archive-and-trend-runs-offline.md)
- [0173 — Own the supervisor a test runs, and bound admission, collection, scratch and
  cancellation by absolute deadlines](0173-own-the-supervisor-a-test-runs-and-bound-admission-collection-scratch-and-cancellation-by-absolute-deadlines.md)
- [0174 — Diagnose in separate child runs that record only the bracketed region, and share one
  test executable boundary](0174-diagnose-in-separate-child-runs-that-record-only-the-bracketed-region-and-share-one-test-executable-boundary.md)
- [0175 — Supervise every diagnostic execution, admit what it wrote, and record what it
  used](0175-supervise-every-diagnostic-execution-admit-what-it-wrote-and-record-what-it-used.md)
- [0176 — Diagnose CLI cases and contained Linux runs, and bound every step and seed before it is
  used](0176-diagnose-cli-cases-and-contained-linux-runs-and-bound-every-step-and-seed-before-it-is-used.md)
- [0177 — Schedule continuous runs from the trusted control, and move builds once to bounded
  shards](0177-schedule-continuous-runs-from-trusted-control-and-move-builds-once-to-bounded-shards.md)
- [0178 — Admit linked diagnostic views by their renderer, and read dispatched branches where
  the checkout fetched them](0178-admit-linked-diagnostic-views-by-their-renderer-and-read-dispatched-branches-where-the-checkout-fetched-them.md)
- [0179 — Link only sanitized views, and bind a diagnostic child to its parent's workloads and
  its frozen recipe kind](0179-link-only-sanitized-views-and-bind-a-diagnostic-child-to-its-parent-workloads-and-frozen-recipe-kind.md)
- [0180 — Hand off published releases read-only, and measure each claimed identity
  once](0180-hand-off-published-releases-read-only-and-measure-each-claimed-identity-once.md)
- [0181 — Declare cohorts before they run, inject only the candidate, and count whole power
  trials](0181-declare-cohorts-before-they-run-inject-only-the-candidate-and-count-whole-power-trials.md)
- [0182 — Bind each calibration member to its first dispatch and its authority's plan, and give
  each probe setup write its own lease](0182-bind-each-calibration-member-to-its-first-dispatch-and-its-authoritys-plan.md)
- [0183 — Detect deleted originals, recompute parameters, and verify activations from retained
  evidence](0183-detect-deleted-originals-recompute-parameters-and-verify-activations-from-retained-evidence.md)
- [0184 — Verify proposed activations in trusted CI, and bind bridges, groups and costs to
  retained executions](0184-verify-proposed-activations-in-trusted-ci-and-bind-bridges-groups-and-costs-to-retained-executions.md)
- [0185 — Measure guarded kernel experiments through a literal bootstrap over a protected
  harness](0185-measure-guarded-kernel-experiments-through-a-literal-bootstrap-over-a-protected-harness.md)
- [0186 — Fold protected paths, key warm targets by the whole Snapshot, bound measure output and
  replan bridge runs](0186-fold-protected-paths-key-warm-targets-by-the-whole-snapshot-bound-measure-output-and-replan-bridge-runs.md)
- [0187 — Bind kernel receipts to their Task results, word reports by what was admitted, and
  retain a real local run](0187-bind-kernel-receipts-to-their-task-results-word-reports-by-what-was-admitted-and-retain-a-real-local-run.md)
- [0188 — Validate kernel receipts as the kernel does, and restore the retained
  history](0188-validate-kernel-receipts-as-the-kernel-does-and-restore-the-retained-history.md)
- [0189 — Read a Task result's sets in the kernel's order, and reconcile a second repair through
  the first](0189-read-a-task-result-s-sets-in-the-kernel-s-order-and-reconcile-a-second-repair-through-the-first.md)
- [0190 — Integrate performance checks, images and evidence
  collection](0190-integrate-performance-checks-images-and-evidence-collection.md)
- [0191 — Keep guarded measure source paths stable by
  content](0191-keep-guarded-measure-source-paths-stable-by-content.md)
- [0192 — Admit single downloads and run CLI measurements
  offline](0192-admit-single-downloads-and-run-cli-measurements-offline.md)
- [0193 — Seed admitted authority fixtures and distribute the full
  suite](0193-seed-admitted-authority-fixtures-and-distribute-the-full-suite.md)
- [0194 — Bind calibration to each case's observed shard CPU
  group](0194-bind-calibration-to-each-cases-observed-shard-cpu-group.md)
- [0195 — State the lines a pull request adds per directory, and check
  them](0195-state-the-lines-a-pull-request-adds-per-directory-and-check-them.md)
- [0196 — Skip pull request measurements only for an explicit `no-perf`
  label](0196-skip-pull-request-measurements-only-for-an-explicit-no-perf-label.md)
- [0197 — Validate on one latest stable Python](0197-validate-on-one-latest-stable-python.md)
- [0198 — Opt this repository into distinct-model Worker independence](0198-opt-this-repository-into-distinct-model-worker-independence.md)
- [0199 — Disable Providers until a machine-local expiry](0199-disable-providers-until-a-machine-local-expiry.md)
- [0200 — Preserve the Provider ID domain and clear orphaned
  disables](0200-preserve-provider-id-domain-and-clear-orphaned-disables.md)
- [0201 — Admit explicit ninety-minute checks and isolate Command catalog
  fixtures](0201-admit-explicit-ninety-minute-checks-and-isolate-command-catalog-fixtures.md)
- [0202 — Wait for complete fixture evidence and diagnose guarded
  failures](0202-wait-for-complete-fixture-evidence-and-diagnose-guarded-failures.md)
- [0203 — Isolate targeted starter repair and retain native exit
  status](0203-isolate-targeted-starter-repair-and-retain-native-exit-status.md)
- [0204 — Preserve model authority in repository planning
  fixtures](0204-preserve-model-authority-in-repository-planning-fixtures.md)
- [0205 — Rerun a failed test alone, report the gate, and retry a failed gate on the same
  candidate](0205-rerun-a-failed-test-alone-report-the-gate-and-retry-a-failed-gate-on-the-same-candidate.md)
- [0206 — Detect Provider logins, and register them through
  `add`](0206-detect-provider-logins-and-register-them-through-add.md)
- [0207 — Key warm check layers by content, lock them, split the build deadline, and seal without
  generated files](0207-key-warm-check-layers-by-content-lock-them-split-the-build-deadline-and-seal-without-generated-files.md)
- [0208 — Recover an Attempt from a transient Provider failure and a reply that does not
  parse](0208-recover-an-attempt-from-a-transient-provider-failure-and-a-reply-that-does-not-parse.md)
- [0209 — Recover a lost or interrupted Attempt without charging its
  reservation](0209-recover-a-lost-or-interrupted-attempt-without-charging-its-reservation.md)
- [0210 — Make the first run say what to do
  next](0210-make-the-first-run-say-what-to-do-next.md)
- [0211 — Give the agent loop its verbs, and deliver a verified
  verification](0211-give-the-agent-loop-its-verbs-and-deliver-a-verified-verification.md)
- [0212 — Continue a Task from its reviewed candidate, and give judging Workers the
  base](0212-continue-a-task-from-its-reviewed-candidate-and-give-judging-workers-the-base.md)
- [0213 — Isolate CLI usage tests from the repository release
  pin](0213-isolate-cli-usage-tests-from-the-repository-release-pin.md)
- [0214 — Integrate Telemetry into the performance laboratory as diagnostics and advisory
  experiments](0214-integrate-telemetry-into-the-performance-lab-as-diagnostics-and-advisory-experiments.md)
- [0215 — Contain telemetry experiments and collect only results verified under their
  authority](0215-contain-telemetry-experiments-and-collect-only-results-verified-under-their-authority.md)
- [0216 — Construct reference fixture bytes without per-byte Python
  loops](0216-construct-reference-fixture-bytes-without-per-byte-python-loops.md)
- [0217 — Bind calibration dispatch to the checked main source and separate group
  arguments](0217-bind-calibration-dispatch-to-the-checked-main-source-and-separate-group-arguments.md)
- [0218 — Reapply an accounting append once after only this writer's heartbeat
  renewals](0218-reapply-an-accounting-append-once-after-only-this-writers-heartbeat-renewals.md)
- [0219 — Own native gate children without leaving the enclosing process
  group](0219-own-native-gate-children-without-leaving-the-enclosing-process-group.md)
- [0220 — Control Store fixture time with a scoped thread
  clock](0220-control-store-fixture-time-with-a-scoped-thread-clock.md)
- [0221 — Integrate the Performance Lab with GA and control short Attempt
  fixtures](0221-integrate-the-performance-lab-with-ga-and-control-short-attempt-fixtures.md)
- [0222 — Fence filesystem cleanup and bound telemetry admission through
  publication](0222-fence-filesystem-cleanup-and-bound-telemetry-admission-through-publication.md)
- [0223 — Retain telemetry work authority before setup and register lock
  ownership](0223-retain-telemetry-work-authority-before-setup-and-register-lock-ownership.md)
- [0224 — Bind diagnostic symbol archives and avoid perf cache
  links](0224-bind-diagnostic-symbol-archives-and-avoid-perf-cache-links.md)
- [0225 — Declare the replacement hosted cohort from the accepted
  producer](0225-declare-the-replacement-hosted-cohort-from-the-accepted-producer.md)
- [0227 — Bound guarded experiment consumers to build and instrument
  inputs](0227-bound-guarded-experiment-consumers-to-build-and-instrument-inputs.md)
- [0228 — Give the guarded improvement fixture a controlled timing
  margin](0228-give-the-guarded-improvement-fixture-a-controlled-timing-margin.md)
- [0229 — Select validation classes and bind release profiles to
  tags](0229-select-validation-classes-and-bind-release-profiles-to-tags.md)
- [0230 — Canonicalize owned performance fixtures and model Linux setup
  explicitly](0230-canonicalize-owned-performance-fixtures-and-model-linux-setup-explicitly.md)
- [0231 — Run Python performance tests after the real
  build](0231-run-python-performance-tests-after-the-real-build.md)
- [0232 — Measure telemetry interruption from the selected fixture
  boundary](0232-measure-telemetry-interruption-from-the-selected-fixture-boundary.md)
- [0233 — Select real AF runtime smoke with the perf
  class](0233-select-real-af-runtime-smoke-with-the-perf-class.md)
- [0234 — Reconcile validation classes with bounded upstream for
  CI](0234-reconcile-validation-classes-with-bounded-upstream-for-ci.md)
- [0235 — Prove release validation without
  publication](0235-prove-release-validation-without-publication.md)
- [0236 — Scope class builds to their
  execution](0236-scope-class-builds-to-their-execution.md)
- [0237 — Isolate telemetry stall boundaries and shortcut uniform
  bootstrap](0237-isolate-telemetry-stall-boundaries-and-shortcut-uniform-bootstrap.md)
- [0238 — Retain refused physical telemetry measurements in incomplete
  results](0238-retain-refused-physical-telemetry-measurements-in-incomplete-results.md)
