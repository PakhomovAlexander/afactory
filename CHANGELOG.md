# Changelog

Every release has a section here before it is tagged: `scripts/release.sh X.Y.Z --compat "…"`
writes it from the pull requests merged since the previous release, and the release workflow
publishes the section as the release notes. The **Authority compatibility** line is mandatory:
it says whether committed `.af/` policy keeps working as is, needs `af onboard --refresh-lock`,
or needs a documented hand edit.

## [Unreleased]

### Upgrading from 0.x

- GA reads only what GA writes
  ([ADR-0113](docs/adr/0113-ga-reads-only-what-ga-writes.md)). Review Campaigns and Tasks that a
  0.x release wrote are not supported (they may be refused or misread): that covers everything
  under `$XDG_STATE_HOME/af/review/` and `$XDG_STATE_HOME/af/task/`, and any directory passed with
  `--state` or `--state-root`. Before upgrading, finish or abandon in-flight Campaigns and Tasks
  with the release that started them, then delete that state. Committed `.af/` files are read only
  in the shapes this release accepts. A key or shorthand that only an earlier release wrote is
  refused: delete the refused key by hand, because `af onboard --refresh-lock` cannot repair a
  file it cannot parse.
- Removed `af review tui`. It read Worker pins only from the lock's legacy `[reviewers]` table,
  so it failed on every lock this release writes. The subcommand is now a usage error, and the
  release no longer ships its `af-review-tui.1` man page.
- Removed `af onboard --migrate` and the `.review/` to `.af/` conversion; the retired `.review/`
  layout is not read at all. The flag is now a usage error. `af onboard` on a repository that
  still carries `.review/` scaffolds `.af/` as for any other repository, a `.review/…` pipeline
  path gets the generic "must live under `.af/pipelines/`" error, and Campaigns whose manifests
  pinned `.review/` paths (af 0.7 and earlier) can no longer be resumed, reported or compiled into
  a Task.
- `.af/af.lock` no longer has a `[reviewers]` table or the 0.7.1 top-level `af_version` key, and
  `.af/af.toml` no longer has `[worker.*]` tables: this release refuses a file that still carries
  them, so delete those lines by hand; `af onboard` no longer writes `[reviewers]` or
  `[worker.*]`. Worker pins live under `[workers]` and the release pin under `[af]`, as before.
  A Worker package's `reviewer.toml` must now declare `subjects`; an omitted list no longer means
  whole-tree only.
- `af review run|plan|render`, the `af review` shorthand and `af provider doctor` no longer accept
  `--authority REV` or `--light`; both are usage errors now. Write `--policy-rev REV`, adding
  `--base REV` for a diff pipeline (a whole-tree pipeline still refuses `--base`), and drop
  `--light`, which only restated the default. The `af/review-plan@1` document no longer carries
  `selectors.compatibility_authority`, and the text plan drops its `compat` line.
- Default Campaign state resolves only the opaque `c-<id>` directory under
  `$XDG_STATE_HOME/af/review/campaigns/`; a directory there named by the label (the layout af 0.4
  and earlier wrote) is no longer a fallback. `af review campaigns|gc --state-root` still list an
  explicit `--state` directory named by its label. A Campaign you placed under that root yourself
  with `--state …/campaigns/<label>` must keep being addressed with `--state`: omitting it starts a
  new `c-<id>` Campaign, and `af review campaigns|gc` then refuse the root because one label holds
  state under both names.
- Removed `af help trust` and its `af-trust.7` man page, which described an `af trust` command that
  never shipped.
- `af self` and `install.sh` no longer install, activate or dispatch to releases older than 0.8.0,
  the first release with a signed `SHA256SUMS`: `af self install 0.7.x` (or a 0.8.0 release
  candidate) is refused, and a project whose lock pins one runs the current `af` instead, with a
  warning. A binary that embeds the release key, and `install.sh` with `minisign` on PATH, now
  refuse any release whose `SHA256SUMS` is unsigned, instead of accepting a pre-0.8.0 release on its
  checksums alone.
- The pre-rename `~/.config/afactory/` directory is no longer read, and nothing warns about it:
  move `providers.toml` and `caches.toml` from there to `~/.config/af/` (or
  `$XDG_CONFIG_HOME/af/`), or `af` finds no provider registry and no cache policy. Setting
  `AFACTORY_CACHE_POLICY_FILE` is no longer an error; it is ignored, so use
  `AF_CACHE_POLICY_FILE`.
- `af task list`, `af task show` and `af task deliver` read only the common `events.sqlite` Task
  store. Implementation Tasks that af 0.8.x and earlier kept in `tasks.sqlite` no longer appear,
  `af task show` no longer emits `af/task-inspection@1`, and delivery no longer knows the
  `refs/afactory/deliveries/<task>` ownership ref. In `af/task-inspection` and `af/task-list`
  output, every delivery preparation and receipt now carries `result_id` and every receipt carries
  `ignored_paths`, as this release always wrote them; the published schema requires both, and a
  Task whose stored receipt lacks one is refused.
- `af task start` now requires `--file`: `--kind implement --goal …` and `--pipeline` are usage
  errors. The fixed implementation v1 format they read, `.af/pipelines/implement.toml` with its
  `.af/workers/` implementer and evaluator packages, is no longer read, and `.af/af.toml` no longer
  accepts `defaults.task_pipeline`: every `af` command refuses a project file that still sets it,
  so delete that line by hand. The `implement` pipeline, its two Worker packages and their pins in
  `.af/af.lock` are then unused and can go too. Run implementation Tasks from a Task file against
  a Task catalog instead; `af catalog init --profile software --destination <new-dir>` creates a
  new starter directory with a runnable catalog and Task files. The `make pilot-check` target is
  gone; `make check` runs the same delivery and recovery tests.
- A Task catalog Worker's `worker.toml` can no longer declare
  `runner.kind = "legacy_task_command"` with its `protocol` and `legacy_budget_tokens` keys: the
  catalog refuses such a package. That runner spoke the fixed implementation v1 Markdown and
  verdict protocol. Declare a `command` or `model` runner instead, which reads
  `af.worker-request/1` and replies with `af.worker-reply/1`. Worker context is always
  `af/TaskContext@1`; `af/TaskContext@2` is neither written nor read.
- Task Review has one generation. A Task catalog whose `[review]` table omits `generation` now
  captures `af.review-task-policy/2`, the same policy as `generation = 2`, instead of generation
  one; any other value is still refused. Reviewer packages must use the generation-two ports: an
  `assignment` input of type `af/TaskReviewAssignment@1`, a `subject` of type
  `af/TaskReviewSubject@2`, and a `review.kernel/ReviewerResult@2` result that lists
  `dispositions` (one per assigned prior Finding) instead of `disputes`. The Review pipeline wires
  each reviewer's `assignment` from `review-bind`. A package with the old
  `af/TaskReviewSubject@1` or `review.kernel/ReviewerResult@1` ports, or without an assignment, is
  refused at planning, so update it together with its pin. The `af/TaskReviewSubject@1` contract
  and its `task-review-subject-v1.json` schema are gone.
- Task execution records have one encoding per record kind: the combined `prepared` record is
  gone, and an Attempt's context is bound through separate `reserved` and `context_bound`
  records. Settlements and usage observations carry decimal-string charges.
  `af/TaskTransition` drops `revision_recorded`, which no release wrote, and
  requires `revocation_id` on `approval_revoked`, which this release always writes.
- `af self optimize` history sources: the `af` adapter reads only the `af/task-inspection`
  receipts that `af task show --json` prints and refuses any other line, including the
  `af.task-event/1` event export that no af command produced. The `af`, `codex` and `claude`
  adapters no longer read normalized records (receipted as `legacy-normalized-v1`): `af` refuses
  such a line, and `codex` and `claude` take no observation from it. Label such a source
  `adapter = "external"` and give it a new `source_id`, because a retained source cannot change
  adapter. Report-only and `--experiment` requests now derive their `optimize-…` Task ID
  the same way light requests do, so re-running one whose capture an earlier release took starts a
  new Task.
- `af/TaskRuntimeEvidence@1` cache observations no longer carry `layer` and `result`, which were
  always `dependency_preparation` and `prepared`, and a runtime span's `kind` is `check` or
  `dependency_preparation` only. `task-runtime-evidence-v1.json`, and the `af/task-inspection`
  schemas that embed it, are narrowed to match, so runtime evidence an earlier release recorded no
  longer decodes.
- Self-optimizer contracts are narrowed in place. `af/OptimizationEconomics@1` drops
  `cache_results`, a per-kind `hit`/`miss`/`unknown` map that duplicated `cache_economics`: read
  `cache_economics.<kind>.hits`, `misses` and `unknown_results` instead.
  `af/OptimizationResult@1` and `af/OptimizationReport@1` drop the constant
  `live_demonstrations: "pending"` field, and optimize Task requirements no longer carry it; the
  Markdown report replaces its "Milestone gates" section with one plain sentence saying live paid
  demonstrations and adoption observations are still pending. A result `conclusion` is
  `validated`, `rejected` or `recommendation_only` (never `inconclusive` or `no_change`), and an
  `af/OptimizationVerification@1` `profile` is always `candidate`. The `af/ExperimentalSlot@1` and
  `af/ExperimentTrialResult@1` contracts and their `experimental-slot-v1.json` and
  `experiment-trial-result-v1.json` schemas are gone. An experiment arm Worker that declares an
  `execution_configuration` input is now refused like any other unavailable input instead of
  having it silently dropped. Optimizer artifacts an earlier release stored may no longer decode,
  and native observations that older captures stored under source-dependent IDs are no longer
  merged, so replaying such a history can count them twice.
- Published schemas are narrowed. `campaign-manifest-v1.json` now requires
  `check_timeout_seconds` and `git_timeout_seconds`, which every current Campaign manifest already
  carries; a manifest from a release that predates them no longer loads. The schemas also drop
  values that no release ever wrote. A dirty `SourceSnapshot@1` capture `boundary` is always
  `revalidated` (`filesystem_snapshot` is gone), and a Gate Execution Binding's
  `provided_isolation` in `run-report-v6.json` and `run-event-v1.json` is `none` or `container`
  (`process` is gone). In `task-contracts-v1.json`, and every schema that
  embeds its Task phase or result, a Task phase is never `resolving`, `planning` or `verifying`, a
  Task result's `execution` is `completed`, `incomplete` or `exhausted` (never `blocked` or
  `cancelled`), and the unreferenced `reviewConclusion` definition is gone.
- A Codex Task Worker's reply is read only from the `-o` last-message file that `codex exec`
  writes (codex-cli 0.147.0 always writes it). The Task adapter no longer falls back to the last
  `agent_message` event on stdout, so with a codex CLI that does not write that file the Attempt
  fails with "Codex Worker returned no final message"; the usage it reported is still charged.
- Source Manifests have one path spelling. The `path_encoding` field (`legacy_v1` or
  `percent_v2`) is gone: every path is spelled the way capture already spelled new trees, so a
  path that starts or ends with whitespace, or holds a space together with a `%` or non-UTF-8
  bytes, is percent-escaped (` notes.md` is `%20notes.md`, `a%b c` is `a%25b%20c`). That now
  includes a file a reviewer or Worker creates during a run, which a sandbox seal, warm workspace
  scan or Task delivery spelled literally when the baseline was an ordinary tree. A Snapshot's
  content digest hashes the stored spelling, so ordinary trees keep their digests, but a tree
  with such a path gets a different Snapshot digest than an earlier release gave it. ADR-0024 is
  superseded by ADR-0113 and removed.
- `af` builds and runs on Linux and macOS only. A source build for any other host, including
  Windows and the BSDs, now stops with a compile error. It no longer compiles fallbacks that
  skipped read-only sandboxes, process-group kills, symlinks or executable bits. The release
  targets and `install.sh` are unchanged.
- `af review report` no longer has a `spend` section: the JSON of every report label,
  `af/review-report@1`, `@3` and `@4`, drops the `spend` array (the `@3` and `@4` schemas no
  longer list it), text output drops its `Spend:` block, and Markdown drops its `## Spend` table
  and `### Attempts` list. They described only Attempts of the pre-Task executor, so for a Round
  a Task hosts they were empty or held a zero-token placeholder row; `task_accounting` reports
  those Rounds' Attempts, usage, wall-clock and caps. `RunReport@1` and `RunReport@2` events
  are neither written nor read, so a Campaign whose log holds one can no longer be run, reported
  or listed. The unused `run-report-v2.json` and `review-report-v2.json` schemas are gone.
- The retired shell review harness is gone from the repository: `compat/legacy-harness/`, the
  `fixtures/synthetic/` corpus generated from it, and the `fixtures/legacy/` private-corpus
  tests. `make fixtures` and `make review-kernel-test-corpus` no longer exist, and `make check`
  no longer regenerates the corpus. A Campaign event log that holds an artifact-less
  `FindingReported@1` (the `"imported": true` shape that only the unused `ledger.jsonl` importer
  wrote) no longer replays, and `af review ledger`, `af review show` and `af review report` no
  longer print an "unavailable: legacy import" placeholder.
- `af review run` and `af provider doctor` run every Campaign on the common Task runtime; the
  pre-Task executor they fell back to is gone. A Campaign whose log holds events only that
  executor wrote (`RunReport@3` to `@5`, Cold Closeout or Session Snapshot events, from Rounds run
  by af 0.9.0-rc.0 or earlier) is refused with "Campaign predates the common Task runtime
  (af < 0.9); start a new Campaign"; one that holds its reviewer Attempt, Provider Operation or
  broker events no longer replays at all (see below). A Campaign whose first Task capture failed
  now retries capture on the common runtime, including after `--restart-round`,
  `af review policy-time advance`, `af review evidence add` or `af review demand waive`, where it
  used to run on the pre-Task executor. `--resume-provider` is gone and is now a usage error
  (exit 2); it only continued that executor's fenced Provider Operations, and the common path
  already refused it. The `af/review-outcome@1` and `af/provider-doctor@1` documents, which only
  that executor printed, are no longer produced: Providers are admitted by the Review Task's own
  probe Attempts, and doctor prints `af/provider-doctor@2`. `af review run` no longer requires
  `HOME` up front. `af review report`, `ledger` and `campaigns` count only Task Attempts toward a
  Campaign's wall-clock, so a pre-Task Campaign's report no longer shows one. ADR-0016 is
  superseded by ADR-0113 and removed. A `--restart-round` before the Task exists now keeps
  Round 1's original prior Finding Set even when the candidate changed, so the Task captured on
  the new epoch resumes; each later run used to fail with "restarted Review changed its original
  prior sets or adjacent epoch".
- The pre-Task executor's Campaign event types are gone from the event vocabulary and from
  `run-event-v1.json`: `AttemptAdmitted@1`, `AttemptDispatched@1`, `AttemptFailed@1`,
  `AttemptFenced@1`, `AttemptFeedback@1`, `AttemptInput@1`, `AttemptReleased@1`,
  `ReviewerExecutionBound@1`, `BrokerOperationCompleted@1` and `ProviderOperationTransition@1`,
  with the `provider-operation-transition-v1.json` schema and the `review.kernel/RefusalHistory@1`
  artifact type. No current command wrote them. A Campaign log that holds one fails to replay with
  "unknown review-kernel event type: <type>; this log was written by another af release; start a
  new Campaign or Task", so `af review run`, `report`, `ledger` and `show` fail on it, and
  `af review campaigns` lists it as a problem. Every event log that holds an event type this
  release does not know fails with the same message. `af review report` no longer carries the
  optional `recorded_not_gathered` field, or prints its "Recorded, not gathered" section, which
  only such events filled; the field is gone from `review-report-v4.json`.
  `af review run` still lists recorded, not gathered results from the Round's Task Attempts. The
  `af review ledger` notice for an absent latest-Round Ledger drops its
  "(N admitted result(s) remain recorded, not gathered)" clause, which always counted 0.
  ADR-0022 and ADR-0023 are superseded by ADR-0113 and removed.
- `RunReport@3`, `@4` and `@5`, the run conclusions only the pre-Task executor wrote, are gone
  from the event vocabulary and `run-event-v1.json`, with the `run-report-v3.json`, `-v4.json`
  and `-v5.json` schemas. `RunReport@6` is the only run conclusion. `run-report-v6.json` now
  defines its outcome, verdict, binding and cache shapes itself, and
  `task-review-gate-facts-v1.json` takes its Cache failure shape from it. A Campaign log that
  holds a retired report, `RunReport@1` to `@5`, no longer replays. When that report is the
  first record replay cannot read, `af review run` and `af provider doctor` refuse the Campaign
  with "Campaign predates the common Task runtime (af < 0.9); start a new Campaign"; when a
  retired Attempt, Provider Operation or broker event comes first, they print the unknown event
  type message above. `af review report`, `ledger` and `show` fail on it, `af review campaigns`
  lists it as a problem, and a new event for the Round such a report concluded is refused with
  the unknown event type message. The Round rows of `af review report` drop `reported_tokens`,
  which only those reports' plain numeric spend filled. Every row now carries
  `task_chargeable_tokens_at_report` and `task_accounting`, which `review-report-v4.json`
  requires.
- The `gate_blocked` suppression reason is gone; only the pre-Task executor's scheduler wrote it.
  A Review Gate is a Task condition, so a node behind a Gate that did not pass reads
  `branch_not_selected` in `af/TaskRunReport@2` and in `af/review-outcome@3` node
  outcomes, or `upstream_missing` once its predecessors were suppressed, and `RunReport@6`
  records both as `upstream_missing`, as before. `task-run-report-v2.json`, `run-report-v6.json`
  and `review-outcome-v3.json` no longer list `gate_blocked`, and the
  review-outcome `ledger_production` no longer lists `not_produced_gate_blocked`. A stored report
  that carries `gate_blocked` no longer decodes.
- A Campaign manifest records only the canonical `report-derived@1` Finding identity policy, and
  `campaign-manifest-v1.json` no longer lists `legacy-path-title@1`. A Campaign whose manifest
  pins that path/title policy (opened before path-independent Finding identity, ADR-0006) can no
  longer be run or continued: its manifest is refused for an unknown finding identity policy, and
  its Ledger reports the manifest as unavailable authority. The Ledger reads a Report only as an
  enveloped `FindingReport@1` whose locations are canonical repository paths. An un-enveloped
  Report, the flat pre-`FindingReport@1` shape, or a Report with a noncanonical location such as
  `./src/a.rs` now projects as an unreadable-authority placeholder that blocks convergence; a
  noncanonical location used to leave the claim readable with unknown Scope. A Ledger node's
  `FindingSet@1` output must be an envelope: the untyped `{round, sources, findings}` summary is
  refused.
- `af review report --json` Finding objects no longer carry `news_round`, a Round counter no
  decision read; convergence counts news by `scoped_news_round`, as before. A Finding view's ID
  hashes the view, so view IDs differ from the ones an earlier release computed for the same
  Finding. A `FindingReport@1` relation only `corroborates` a `finding`: the `disputes` kind and
  the `report` target, which no release wrote, are gone from `finding-report-v1.json`, and a
  Report that uses them no longer decodes.
- Review pipeline format 1 is gone. A pipeline that declares `version = 1`, the format without
  `[subject]` whose untyped `findings`, `prior_findings` and `change_set` ports were typed by their
  names, is refused as an unsupported version; formats 2 through 5 are unchanged. Declare
  `version = 2` with `[subject]` and typed `FindingSet@1` or `ChangeSet@1` ports instead. A
  Campaign whose manifest pinned a format 1 pipeline can no longer be resumed or continued.
- Campaign review has one reviewer contract, `review.kernel/ReviewerResult@2`. A pipeline is
  refused when it loads if a reviewer declares `review.kernel/ReviewerResult@1` or an untyped
  result output such as `outputs = ["result"]`, or if its Generation emits
  `review.kernel/PriorFindings@1`. Every reviewer and every Scatter must declare one optional,
  singular `review.kernel/FindingSet@1` input with snapshot affinity `any`, wired from
  Generation's `FindingSet@1` output. A reviewer without it used to run as `ReviewerResult@1`,
  and a Scatter without it fell back to `@1` silently; both are now refused at plan time.
  Reviewers answer `dispositions`, one per assigned prior Finding (`corroborate`,
  `not_reproduced` or `dispute`, keyed by `finding_id`), instead of `disputes` keyed by
  `claim_id` with `confirm` or `refute`. `af onboard` and the software starter already write this
  wiring; update a hand-written pipeline and its pin in `.af/af.lock`. The
  `reviewer-result-v1.json` schema and its conformance corpus are gone, and
  `reviewer-result-v2.json` now defines the flat report shape itself. `finding-set-v1.json` accepts
  only `review.kernel/finding-reducer@2` and `task-review-result-metadata-v1.json` only
  `ReviewerResult@2`, so a stored `ReviewerResult@1` result or a Finding Set reduced by
  `finding-reducer@1` no longer loads; a Ledger that reduces no reviewer result now records
  `finding-reducer@2` as well, so that Finding Set's ID differs from an earlier release's. A
  command reviewer's stdin document now always carries
  `"result_contract": "review.kernel/ReviewerResult@2"`.
- `review.kernel/ReviewerResult@2` no longer has the reviewer's `verdict` and `summary`, which no
  decision, report or display read. A result is exactly `reports`, `benchmark_demands` and
  `dispositions`, and the model output contract no longer asks for the other two. A model reviewer
  that still sends them is unaffected, because its answer is normalized to the contract. A command
  or Task reviewer Worker whose reply still carries them is refused as
  `unexpected_or_missing_fields`: drop both keys from its output and from the package's
  `outputs/result.schema.json`, then re-pin the package digest. That includes the `bugs` and
  `correctness` Workers of a starter an earlier `af catalog init --profile software` wrote; run
  it again into a new directory to get the current ones.
- Every review pipeline port is a typed table, in every pipeline format. The string shorthand
  (`outputs = ["decision"]`, `inputs = ["reports"]`) is refused when the pipeline is parsed, and so
  is a node without `outputs`, which used to get an implicit `out` port. A Gate, Gather or Ledger
  output typed `review.kernel/Opaque@1`, the type the shorthand stood for, is no longer retyped by
  its node kind: it is refused as an unsupported Review output before the Round's Gate runs, and
  the Store no longer skips payload validation for Opaque@1 artifacts. Spell each port out as
  `{ name = "…", type = "…", cardinality = "one", optional = false, snapshot_affinity = "any" }`:
  a Gate outputs `review.kernel/GateDecision@1`, a Gather `review.kernel/ReportSet@1`, and a
  Ledger exactly one `review.kernel/FindingSet@1`, optionally beside a `review.kernel/DemandSet@1`;
  a Ledger without that one Finding Set output is refused when the pipeline loads, where a lone
  untyped Ledger output used to receive the Finding Set by position. `af onboard` and every
  shipped pipeline already write typed ports. A Campaign whose manifest pinned a pipeline with the
  shorthand can no longer be resumed or continued.
- Brokered credentials are gone; GA has no broker. A format 4 or 5 reviewer's `execution` accepts
  only `credential_mode = "credential_free"` or `"trusted_unsafe"` and `auto_apply`:
  `credential_mode = "brokered"` and an `operations` list are refused when the pipeline is parsed,
  in `af review plan` too. They used to pass `plan` and fail only at `af review run`, because no
  adapter could serve them. No shipped pipeline declares either. Provider admission always
  compiles to `af/TaskProviderAdmission@1` with the `af/TaskProviderContext@1` readiness context.
  The captured `af/LegacyReviewTaskPolicy@4` drops `settings.provider_probes`, which was always
  empty, and holds the review settings directly under `settings`, so a new Review Task's policy
  digest differs from an earlier release's. The `task-provider-admission-v2.json`,
  `task-provider-context-v2.json` and `task-provider-probe-policy-v1.json` schemas are deleted, and
  `compiled-task-v1.json` loses the `provider_admission_brokered` operator.
- The Broker's Task records went with the Broker; no release wrote them, because none ever
  installed a Broker. `TaskBrokerTransition@1` is no longer an event type, so a Task log holding
  one fails to replay. `af task show --json` and `af task explain --json` no longer emit
  `af/task-inspection@4`, a `broker_records` section or a `broker_transition` history entry, and an
  `af self optimize` history source with the `af` adapter refuses an `af/task-inspection@4`
  receipt. The `task-broker-binding-v1.json`, `task-broker-operation-v1.json`,
  `task-broker-transition-v1.json`, `broker-operation-receipt-v2.json` and
  `task-inspection-v4.json` schemas are deleted, and `run-event-v1.json` and
  `task-inspection-v11.json` drop their Broker entries.
- Task inspection has one version. Every command that prints a Task as JSON (`af task show`,
  `explain`, `plan`, `start`, `run`, `approve` and the others, and `af self optimize`) now emits
  `af/task-inspection@11`, instead of a version from `@3` to `@11` chosen by the sections the Task
  happened to have. Each section beyond the core (`owned_child_sets`, `review_handoffs`,
  `review_integrations`, `attempt_walls` with `runtime_observations`, `experiments` and
  `adoption_observations`) appears only when the Task recorded it, and `history` holds
  `TaskTransition@5` payloads. `task-inspection-v11.json` is now self-contained and
  describes all of it; it no longer requires `experiments` and `adoption_observations`, and a
  Failed settlement's `diagnostic` must be a JSON object, as this release always writes it. The
  `task-inspection-v3.json` and `-v5.json` to `-v10.json` schemas are deleted, and
  `task-list-entry-v2.json` and `task-plan-inspection-v1.json` now refer to
  `urn:af:schema:task-inspection:11`. The `af` history adapter of `af self optimize` accepts only
  `af/task-inspection@11` receipts, so a receipt an earlier release exported is refused. A script,
  skill or hub check that matches another `af/task-inspection@N`, or validates against a deleted
  schema, must switch to `@11`.
- Task usage and Review provenance have one encoding each. Every usage artifact is
  `af/TaskTokenUsage@3` and every Task Review provenance artifact is
  `af/TaskReviewAttemptProvenance@2`, whatever the width of their counters; narrow values no
  longer select `af/TaskTokenUsage@1` or `@2` or `af/TaskReviewAttemptProvenance@1`. The
  payload bytes are unchanged, but the artifact types and content IDs of new usage and
  provenance artifacts differ from those an earlier release wrote. A Task Review selection whose
  provenance or usage artifact carries a retired type, or no artifact envelope at all, is
  refused, and the `task-token-usage-v1.json`, `task-token-usage-v2.json` and
  `task-review-attempt-provenance-v1.json` schemas are deleted: every other schema now refers to
  `urn:af:schema:task-token-usage:3` for its decimal counters.
- Task records have one version per name. `TaskTransition@1` to `@5` collapse to
  `TaskTransition@5`, `af/TaskExecutionRecord@1`, `@3`, `@4` and `@5` to
  `af/TaskExecutionRecord@5`, `af/TaskRunReport@1` and `@2` to `af/TaskRunReport@2`, and
  `af/TaskReviewHandoff@1` and `@2` to `af/TaskReviewHandoff@2`. Every change kind, record kind
  and field is kept: the new versions are supersets of the old ones, so a transition now carries
  `review_continued`, `review_integration_selected`, `review_integration_finished`,
  `recording_resumed` or `adoption_observation_recorded` under the same number as `opened` or
  `finished`; a settlement or usage observation carries its decimal-text `charged_tokens`, and
  the owned-child and experiment kinds travel in the same record type. An Integration phase
  report is `af/TaskRunReport@2` with a `phase_id`; a Round report is the same type without one.
  The `task-transition-v1.json` to `-v4.json`, `task-execution-record-v1.json`, `-v3.json` and
  `-v4.json`, `task-run-report-v1.json` and `task-review-handoff-v1.json` schemas are deleted,
  and `run-event-v1.json` and `task-inspection-v11.json` name only the surviving versions. This
  is a wire break: a `.af/state` Task log or CAS record an earlier release wrote no longer
  decodes, and `af task list` and `af self optimize` fail for the whole store while one remains,
  rather than skipping it. Finish or delete in-flight Tasks before upgrading. The
  `execution_records[].artifact_type` and `review_handoffs[].artifact_type` values in
  `af task show --json` each collapse to one string.
- A Task catalog and its captured run authority have one schema each: `af.task-catalog/2` and
  `af.task-run-authority/2`. `provider_admission` is now optional in a catalog; omitting it means
  the fixed 4,096-token, 45-second admission allowance that `af.task-catalog/1` had, and
  declaring it keeps the explicit bounded cost. A committed `.af/task-catalog.toml` that still
  says `schema = "af.task-catalog/1"` is refused with `Task catalog requires schema
  af.task-catalog/2`; change that one line by hand (nothing else about the file changes).
  `af catalog init` now writes `af.task-catalog/2` for every profile. The captured run
  authority always records the resulting admission cost and re-checks it against the captured
  catalog bytes, and `task-catalog-v1.json` is deleted.
- The Attempt wall sidecar in `events.sqlite` keeps one exact usage column, `usage_v3_json`,
  beside `usage_observation_v1_json`; both are created with the `attempt_wall` table. The numeric
  token columns, `usage_v1_json`, `usage_v2_json` and the `ALTER TABLE` migration that ran on
  every open are gone, as is the Campaign-keyed narrowing view only the removed pre-Task executor
  wrote. Wall rows an earlier release recorded in the retired columns are no longer read, so
  finish in-flight Tasks before upgrading: recovery raises an abandoned Attempt's charge from its
  recorded wall usage, and without it the Attempt settles at its reservation floor.
- `af review run --json` always emits `af/review-outcome@3`, instead of `@2` when every selected
  Attempt's usage fitted u64, and `review-outcome-v2.json` is deleted.
- `af review report --json` always emits `af/review-report@4`, instead of `@3` for narrow usage or
  `@1` for a Campaign with no Task. `task_accounting` is always present; it is empty exactly for a
  Campaign whose first Task capture failed, which has no Rounds either, and
  `review-report-v4.json` describes that case. `review-report-v3.json` is deleted. A script, skill
  or hub check that matches `af/review-outcome@2`, `af/review-report@1` or `af/review-report@3`,
  or validates against a deleted schema, must switch to the single version.
- The reviewer output contract a model Worker is prompted with now names its report array
  `reports`, the key the stored `ReviewerResult@2` artifact and every Worker package schema
  already used, instead of `findings`. An answer that still says `findings` is read as `reports`,
  so hand-written command reviewers keep working, but the prompt bytes changed: reviewer Attempt
  context IDs differ from those an earlier release computed for the same node.

## [0.9.0-rc.6] - 2026-09-21

### Authority compatibility

Prerelease: committed .af authority keeps working as is and needs no migration. The sha2 0.11, rusqlite 0.40 and jsonschema 0.56 updates change no stored spelling: Attempt IDs, sha256: artifact addresses, receipt prefixes and local state directory names are byte-for-byte what earlier releases wrote, and review_core::hex::encode pins that encoding under test. The machine-local Provider registry stays version 1 — af provider setup and af provider recover run in the invoking release instead of dispatching through a repository pin, and a replacement is published by one atomic exchange, so a supported pinned reader sees either the complete old registry or the committed new one (ADR-0111). The one change that can stop a working machine: Provider admission now revalidates auth directories, so an existing symlinked, foreign-owned, or group/world-writable auth directory or registry is refused until its ownership and permissions are tightened. Refresh a consumer pin explicitly with af onboard --refresh-lock after verifying the release and its archive digests, and keep the previous release for rollback.

### Changes

- release: v0.9.0-rc.4 (#98)
- build(deps): bump actions/checkout from 4.2.2 to 7.0.1 (#82)
- build(deps): bump actions/upload-artifact from 4.6.2 to 7.0.1 (#81)
- build(deps): bump actions/download-artifact from 4.3.0 to 8.0.1 (#80)
- Take the pending dependency updates, adapting digest and SQLite identities (#99)
- build(deps): bump clap_mangen from 0.2.33 to 0.3.3 (#87)
- build(deps): bump rusqlite from 0.37.0 to 0.40.2 (#86)
- build(deps): bump sha2 from 0.10.9 to 0.11.0 (#85)
- Bump nix from 0.30.1 to 0.31.3 (#84)
- build(deps): bump jsonschema from 0.26.2 to 0.56.0 (#83)
- Make fresh Provider onboarding copyable (#90): make fresh Provider bootstrap one command with
  `af provider setup`, keep `af provider add` for already-authenticated contexts, isolate login
  environment and auth-directory ownership, make setup serialize by canonical auth context,
  distinguish ambient discovery labels from selectable registry IDs, preserve every Gate and
  executing release in onboarding's copyable apply command, make registry publication serialized,
  conditional, durable and atomic across pinned releases, secure its directories and files
  independently of `umask`, reject unpublishable auth paths before login, bind security-sensitive
  operations to stable directory handles, provide hash-validating fail-closed recovery with
  `af provider recover`, and make the README's Codex-only quickstart complete.
- Skip the last non-UTF-8 path test where the filesystem refuses such a name (#106)
- release: v0.9.0-rc.5 (#100) — tagged but never published: its macOS check leg failed, so
  everything it carried ships here instead.

## [0.9.0-rc.5] - 2026-09-20

Tagged but never published — the release workflow's macOS check leg failed before the
publish step. Everything this version carried ships in 0.9.0-rc.6.

## [0.9.0-rc.4] - 2026-09-20

### Authority compatibility

Prerelease: committed .af authority keeps working as is. Every warm layer is opt-in per reviewer node (warm = { notes, build_cache, workspace, session }); a pipeline that declares none behaves exactly as before, and the Store, the run events and the pinned authority mirrors gain additive records only, with no migration. The session layer is Claude-only and defaults to off: Codex reviewers and Task-hosted Review Attempts record a drop reason and run on Notes instead. A candidate-built build cache is an explicitly unsafe artifact, never a Cache Snapshot, and is refused under the safe policy at load, at selection and at capture. WarmSetSelected@1 first ships here, so its vocabulary closes with this release (ADR-0107). Refresh a consumer pin explicitly with af onboard --refresh-lock after verifying the release and its archive digests, and keep the previous release for rollback. No cold-versus-warm savings Evidence exists yet, so the design review's build-minute and forked-resume Demands stay open and both session and workspace policy defaults ship off.

### Changes

- release: v0.9.0-rc.2 (#93)
- Add self-optimizer economics, experiments and light optimization (#94)
- release: v0.9.0-rc.3 (#95)
- Reduce release latency with shared validation and concurrent builds (#96)
- Start Workers warm from declared layers and confirm clean Rounds cold (#97)
### Changes

- Add the first Worker warm layer: a reviewer node with `warm = { notes = true }` asks each
  admitted Attempt for bounded Worker Notes, carries them to the next Round's Attempt of the same
  node as `review.kernel/WorkerNotes@1`, marks every path against the previous head in a
  `review.kernel/HeadDelta@1`, and records the selection as `review.kernel/WarmSet@1` with
  `WarmSetSelected@1` before dispatch. Notes are parsed beside the flat Reviewer Result, dropped
  with a recorded `WorkerNotesRecorded@1` reason when malformed or over `notes_max_bytes`, and
  rendered as data with their own context manifest entries; `af review report` shows the layers
  and rendered input per Attempt. Task Workers may declare optional `af/WorkerNotes@1` ports that
  the compiler wires only within one slot. With `warm` absent nothing changes
  ([ADR-0107](docs/adr/0107-carry-worker-notes-and-head-deltas-as-declared-warm-layers.md)).
- Carry the Gate's build to Worker sandboxes as the second warm layer: a `trusted_local` Gate
  that declares `build_caches = ["cargo_target"]` builds into the reserved `.af-cache` root with
  `CARGO_TARGET_DIR` pointed at it, captures the result after its checks pass as an explicitly
  unsafe `review.kernel/BuildCache@1` (regular files only, no-follow traversal, entry, depth,
  path and byte limits, fixed modes, stripped xattrs and ACLs, producer and head provenance),
  and records `BuildCacheCaptured@1` with the artifact or a refusal reason. A reviewer node with
  `warm = { build_cache = ["cargo_target"] }` receives a per-Attempt clone from the CAS through
  its `WarmSet@1`, the bytes are removed before seal so the sealed diff equals a cold run's, and
  the safe policy refuses the declaration at load and the handoff before any dispatch. The
  registry-only `cargo` Cache Snapshot and the self-optimizer cache path are unchanged
  ([ADR-0108](docs/adr/0108-carry-gate-build-caches-as-explicitly-unsafe-warm-layers.md)).
- Add the Warm Workspace as the third warm layer: a reviewer node with
  `warm = { workspace = "rebase" }` keeps one stable template root per Campaign under
  `$XDG_CACHE_HOME/af/workspaces`, named in its `WarmSet@1` by an opaque workspace identity
  rather than a host path. On a new head the kernel applies the tree diff to a copy-on-write
  clone of the previous template, scans the result and swaps it in only when its manifest digest
  equals the head's Tree Digest; any other outcome falls back to a full materialization from the
  CAS with the reason recorded. An unchanged head materializes nothing and is verified by a read-only scan before it is reused; the root's marker is trusted only against the Campaign log's last `WorkspaceRebased@1`, and a marker the log never recorded rebuilds the head as `unrecorded_preparation`. Preparation failures carry no host path, cold pipelines touch no cache configuration, and the event records the preparation time. `WorkspaceRebased@1`
  records the previous and current head, the basis, the fallback reason, the verified digest and
  the entries touched before the Warm Set is recorded, and the `workspace` layer joins
  `WarmSetSelected@1` when the template was carried. Per-Attempt sandboxes remain fresh clones,
  so a warm Attempt's sealed diff is what the reviewer wrote; nodes without the policy and
  pipelines written before it are unchanged
  ([ADR-0109](docs/adr/0109-rebase-warm-workspaces-at-stable-roots-with-digest-verification.md)).
- Add the Session Snapshot as the fourth warm layer, for Claude reviewers only, and the compiled
  Cold Closeout beside it. A node with `warm = { session = "if_recent" }` runs each Attempt under
  a `--session-id` the kernel derives from the Attempt ID; at seal the bounded transcript enters
  the CAS as `review.kernel/SessionSnapshot@1`, `SessionSnapshotPrepared@1` records it with the
  bytes, the estimated tokens and a path-free source identity, the harness copy is deleted, and
  `SessionSnapshotCleaned@1` closes the protocol. What is stored carries no host path and no
  credential: the sandbox and harness paths become reversible placeholders a materialization puts
  back, and a transcript carrying a credential shape is refused whole. Every component below the
  granted harness root is opened `O_NOFOLLOW`, and the directory a transcript was validated in
  stays open through its unlink. An Attempt that ends any way but an admitted capture deletes its
  own transcript before its retry. A sweep before the
  Round's first Attempt finishes any cleanup a crash interrupted and removes every transcript the
  node's Attempts could have left, without a provider call. The next Round re-materializes the
  transcript and resumes it with `--resume --fork-session`, sending only the delta prompt — no
  package instructions, no Change Set patch — with both the transcript and the delta listed in the
  Attempt's context manifest. Provider support, `warm.session.max_age` and fitting the reservation
  beside the delta are gates whose every failure is a recorded `WarmSet@1` drop back to Notes
  alone, including a Head Delta dropped over its bound; Codex implements nothing and keeps
  `--ephemeral`. `[convergence] cold_closeout` compiles a conditional cold Attempt of a warm
  reviewer, reserved before its warm Attempt so a retry cannot consume it, dispatched at the
  Ledger only when every warm result of the Round would otherwise close it clean, run through the
  ordinary Attempt lifecycle under its own closeout slot, and folded through
  `ColdCloseoutDispatched@1` as a stage of its own before the convergence decision. A confirmation
  that produced no admissible result leaves the Round incomplete.
  Both policies default to off, so a pipeline written before this package is unchanged
  ([ADR-0110](docs/adr/0110-capture-sessions-in-two-phases-and-confirm-clean-rounds-cold.md)).

## [0.9.0-rc.3] - 2026-09-17

### Authority compatibility

Prerelease: existing .af authority remains supported. Self-optimization is opt-in and requires a reviewed project optimizer catalog, bounded policy and the RC3 binary; generated experiments still require exact signed developer approval. Refresh a consumer pin explicitly with the supported onboard refresh-lock path after verifying the release. New optimizer state and Task inspection/transition formats require a compatible binary; preserve the prior Store and known-good release for rollback. Heavy redesign, live paid savings demonstrations, longitudinal adoption and stable-release migration/pilot gates remain pending. This release does not switch consumer pins or claim stable readiness.

### Changes

- Add project-history capture and reporting with explicit token, elapsed-time and cache evidence (#94).
- Run bounded optimization experiments with signed approvals, independent evaluation and exact accounting (#94).
- Add light optimization for Worker instructions and safe Cargo cache selection, with payoff gates and durable adoption evidence (#94).
- Exercise candidate model contexts and delivered cache settings through real Task paths; coordinate the completion-order test explicitly to avoid scheduling flakes (#94).
## [0.9.0-rc.2] - 2026-09-15

### Authority compatibility

Prerelease: `af task start` now captures and previews without dispatching Workers, including
with `--json`. Review the plan, then run with `--confirm-plan` and its full captured ID.
Automation must explicitly opt into `--execute` on start or on the first run of a plan.
Admitted Tasks can resume and finished Tasks replay as before. This CLI confirmation never
replaces signed developer approval for a generated plan. Existing `.af` authority remains
supported; legacy `.review` migration and exact release pin verification still apply.

### Changes

- Render captured Task plans as compact ASCII flows or expanded `task explain --tree` views,
  with embedded calls, actual Worker models/efforts/accounts, inputs, outputs, effects and limits.
- Stop new Tasks before execution and refuse stale plan confirmations, including a plan change
  before the writer lease is acquired. Explicit automation retains existing runtime admission.
- Keep JSON inspection schemas unchanged, sanitize terminal display text, and document the
  preview/approval workflow for Claude and Codex.

The live pilot and complete consumer migration/rollback acceptance remain pending. This
candidate does not declare stable-release readiness or change the active consumer's pin.

## [0.9.0-rc.1] - 2026-09-15

### Authority compatibility

This is a prerelease for Task integration and migration validation. Live pilot acceptance
and complete consumer migration/rollback verification remain pending; this candidate does
not establish stable-release readiness or replace the known-good consumer installation.

New Task execution requires its versioned `.af` catalog, Pipeline/Worker contracts and lock.
Every generated Execution Plan requires an authorized developer's exact-plan approval.
Local bindings do not travel with shared definitions. Existing `.review` consumers still
need the supported `af onboard --migrate --apply` path; validate migrated authority together
with the chosen released binary and its verified archive digests before switching launchers.

New Review executions use the common Task runtime. Historical paid Campaigns preserve their
captured executor, authority and accounting; they are not rewritten into new Tasks. Keep the
original Store and known-good release for historical continuation and rollback. An older
binary must not reinterpret unsupported new state, and rollback cannot refund recorded usage
or reopen completed work. Release-bound migration and rollback evidence are release gates.

### Changes

- Make Task the common durable execution model for implementation, Review and documents,
  with typed Pipeline input/output contracts, captured context and shared budget accounting.
- Compose reusable Review Pipelines inside implementation, with independent acceptance and
  bounded repairs that retain Finding and Snapshot provenance.
- Select fitting shared Pipelines; persist generated definitions and plans for developer
  review when configured generation is needed; export definitions for subsequent reuse.
- Share versioned Pipeline/Worker packages and starter workflows through Git, with local
  Provider bindings, deterministic contract checks, Jira/local source capture and explicit
  delivery to a new local worktree.
- Preserve Provider usage and recovery evidence across cancellation, writer loss and storage
  failures, and capture explicit admission allowances in Task catalog V2.
- Preserve nested Jira requirements, resolve explicit relative refresh files from the caller's
  directory, and retain plans when only unselected Jira fields or update timestamps change.
- Make identical authenticated plan-revocation requests idempotent and refuse revocation of
  decisions that were never approved.
- Reduce repeated captured Review validation, heartbeat and receipt-query work while retaining
  fresh artifact, lease, approval and usage checks; stream raw usage transcripts with bounded memory.
- Open the repository: `install.sh` installs from a public release with `curl` alone, verifies
  the release signature by default when `minisign` is present, and keeps `gh` only as a
  fallback; the release also ships `LICENSE` (Apache-2.0), `SECURITY.md`, `CONTRIBUTING.md`
  and a restructured `docs/` (architecture, tasks, migration, non-goals, design notes).

- Account for every model reported by native Claude Task usage; preserve auxiliary charges
  and refuse unapproved auxiliary activity instead of accepting an Opus selector alone.
- Use native JSON schemas and structured output for typed Claude Task replies while retaining
  independent Kernel output validation and the existing legacy text transport.
- Preserve prerelease versions in migrated authority so the candidate can read its own output.
- Retry transient executable-busy process starts and correct concurrent Linux test allowances.
- Explain logged-out Provider status and install declared toolchain components in release jobs.

### Validation boundary

The release workflow checks the tagged source on Linux and macOS, runs consumer fixtures,
and signs archive checksums. These deterministic checks do not replace specialist reviews,
live Task measurements or actual consumer migration and rollback rehearsals. See
[Task execution](docs/task-execution.md) for the contracts; candidate publication does not
claim those remaining acceptance gates have passed.

## [0.8.0] - 2026-09-08

### Authority compatibility

Requires `af onboard --migrate --apply` for a consumer still on `.review/`: legacy authority is
no longer read for new Campaigns (ADR-0043). A repository already on `.af/` keeps working, but
re-pin it with `af onboard --refresh-lock` so the lock records the per-target archive digest this
release binds to. A `0.7.1` default cannot read a lock written by `0.8.0` (its `[af]` table is an
unknown field to the older parser), so it neither dispatches to `0.8.0` nor plans: run
`af self update` first on such a machine.

### Changes

- Route pipelines by changed paths; switch oversized Diffs to a bounded pipeline (#64)
- Show Campaign state on disk and reclaim it with af review gc (#66)
- One release train, a pin that binds bytes, and .review/ retired (ADR-0045) (#68)
- Commit the minisign release public key: every release from 0.8.0 ships a signed `SHA256SUMS`

## [0.7.1] - 2026-09-03

### Authority compatibility

Unchanged; `.af/af.lock` now records the `af` release that wrote it (`af onboard --refresh-lock`
re-pins). Legacy `.review/` policy is still read and upgraded in place by `af onboard --migrate`.

### Changes

- Plan consumer policies with every built af (#48)
- Validate and migrate legacy .review/ policy in af onboard (#49)
- Refresh the hub consumer fixture: one correctness reviewer (#51)
- Make wall-clock, provider usage, and dispositions visible per review (#54)
- Let a Worker node declare its own Attempt cap (#60)
- Render a Worker's exact input token-free (#61)
- Refuse a Worker input that exhausts its Attempt cap before admission (#62)
- Make af self-managed: clap tree, scoped help, completions, af self, dispatch, config ladder (#63)

## [0.7.0] - 2026-09-01

### Authority compatibility

The Ledger node must declare a `review.kernel/DemandSet@1` output; `af onboard --migrate --apply`
adds it to a legacy pipeline.

### Changes

- Harden review planning and provider admission (#31)
