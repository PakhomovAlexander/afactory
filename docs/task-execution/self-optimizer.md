# Project economics and the light optimizer

`af self optimize` is the M1 project-economics entry point. Unlike binary-management `self`
commands it dispatches through the repository's `.af/af.lock`, then performs a token-free stable
prefix capture and previews a normal common-runtime Task plan. Run the exact captured plan with
`af task run TASK_ID --confirm-plan PLAN_ID`, or use `--execute` for explicit automation.

The project catalog must map kind `optimize` to an `optimization_analysis` kind package and select
a Pipeline with this public contract:

```text
history   af/OptimizationHistory@1  -> operator/optimization-project
economics af/OptimizationEconomics@1
report    af/OptimizationReport@1   (covers analysis)
```

The operator is installed by the engine, performs no model call, has no effects and uses the
common Task log and accounting. `af task output TASK_ID --port report --format markdown` invokes
the typed renderer; it does not reinterpret or upgrade the report.

History sources are explicit in `.af/optimization-sources.toml` (or `--history-config`):

```toml
schema = "af.optimization-sources/1"
project_id = "sha256:..."
max_sessions = 200
max_raw_bytes = 268435456
max_record_bytes = 1048576
max_normalized_bytes = 16777216

[[sources]]
    adapter = "af" # native AF event export; codex | claude | external are also supported
path = "../captured-history/af.jsonl"
source_id = "af-task-store-export"
execution_id = "project-history"
```

`external` is the explicit normalized import and fixture adapter. Each JSONL line has
`observed_unix_ms`, exact `attribution`, and optional typed `tokens`, `spans`, `caches`, `outcome`,
and `missing_fields`; unknown fields are rejected. Historical files that used another adapter
label for this shape remain readable and are receipted as `legacy-normalized-v1`.

`af`, `codex`, and `claude` consume their declared native JSONL shapes. Their parsers select only
project/session identities, timestamps, model/effort labels, exact usage counters, lifecycle,
cache receipts and outcome authority. Message text, tool payloads, headers, environment values,
thinking and redacted-thinking blocks are never copied to an Observation. AF records require an
exact `project_id`; provider sessions require either that identity or a canonical `cwd` equal to
the selected repository. Foreign and unassigned sessions are excluded and reported as coverage
gaps. Codex cumulative `token_count` snapshots and repeated Claude message snapshots share stable
cumulative keys, while distinct turns/messages remain distinct. Provider-native costs are marked
as outer-session usage and never added to AF charges.

Credential-free examples of all three formats live in `fixtures/self-optimizer/native/`. AF event
exports join `attempt_started`, `usage_observed`, and `attempt_settled` by exact Attempt identity;
the joined lifecycle creates concurrent-safe host spans, and declared cache observations retain
eligibility, hit/miss/unknown, temperature, invalidation identity and reuse measurements. Fields
that the historical source did not measure remain named unknowns. No provider-internal span or
cache result is inferred.

Every source file is read only to a complete bounded record under a stable size/mtime check. The
receipt records the exact byte range, prefix digest, UTC cutoff, adapter/redaction version and
completeness. A filter, foreign-project exclusion, incomplete final record or unsupported identity
makes coverage visibly partial.

Without a collection selector, capture resumes each declared source after the latest retained
receipt. `--since 30d` filters by the captured cutoff; `--all-history` starts each range at zero.
Partial and unavailable sources become visible gaps. Appended captures reference their exact
predecessor; projection deduplicates exact observations and cumulative snapshots without erasing
distinct repeated failures. Case families disclosed by the report are retained as exposed and
are not fresh holdout evidence for later milestones.

An analysis-kind catalog remains report-only. A candidate-kind catalog that installs
`builtin/optimization-light` and a captured `.af/optimization-policy.json` makes
`--strategy light` select the M3 path: deterministic profiling, one diagnosis Attempt, one
proposal Attempt, protected preparation, the separately approved experiment closure, independent
evaluation and verified local delivery. Diagnosis and proposal consume only the data-only
`OptimizationProfile@1`, recipe catalog, aggregate economics, Requirements and
`OptimizationWritableConfiguration@1`. The last contains only bounded UTF-8 files below the
policy's writable roots, with exact content identities and explicit omissions. These Workers have
no source port and run in an empty read-only sandbox; protected case bodies, holdout labels and the
unrestricted SourceTree stay available only to trusted candidate construction and verifiers.

The initial executable recipe hooks cover context/retrieval deduplication and admitted Cargo
dependency preparation. For a context candidate, `OptimizationExecutionConfiguration@1` is a
kernel-only derivation blueprint. Before the signed closure is prepared, AF derives a new captured
`TaskPackage@1` whose only changed byte is `instructions.md`; the candidate child names and runs
that package through the ordinary host while the baseline runs the original package. The blueprint
is not a business input, and the native model context therefore contains either the old or new
instructions, never both. Store registration rechecks the original package, candidate Snapshot,
repin, exact instruction content, unchanged manifest/contract/runner and registered invocation.
Local delivery contains the exact measured candidate package bytes.

A cache candidate writes only `.af/cache/cargo.json` with the fixed non-executable value
`{"schema":"af.sandbox-cache-selection/1","kind":"cargo"}`. The shared code-Task environment
consumes that same captured selection for both candidate trials and later ordinary non-writing
command checks. It resolves only an administrator-approved Cargo snapshot from
`AF_CACHE_POLICY_FILE`, hashes and copies it under existing bounds, sets sandbox-local offline Cargo
variables, records lookup/copy evidence, and removes it before source sealing. Model Workers and
source-writing Workers cannot consume this command environment. The invalidation identity retains
the current source Snapshot, selection bytes, admitted policy, actual cache-source digest and a
captured Rust toolchain declaration when present; engine identity is not substituted for missing
toolchain evidence. This is dependency-preparation evidence, not a compiler cache-hit claim.

Provider prompt-cache control, targeted retry configuration and Worker binding tuning remain
explicitly unsupported until their exact installed hooks exist. Deterministic artifact reuse is
also unsupported: a current-CAS receipt without a reachable consumer does not avoid work. A stable
prefix, receipt, or changed source Snapshot is not proof that a candidate configuration ran.

The repository ships deterministic command test doubles and production model packages. A
reviewed shared catalog should map `builtin/optimization-light` to
`fixtures/self-optimizer/catalog/optimization-light-model`, and include the two
`optimization-light-*-model` Workers plus protected arm/evaluator packages. Validate and install
that committed catalog through the supported capture path (these commands dispatch no Workers):

```sh
af catalog test --source ../afactory --revision HEAD \
  --manifest path/to/reviewed-optimizer/catalog.toml --json
af catalog sync --source ../afactory --revision HEAD \
  --manifest path/to/reviewed-optimizer/catalog.toml \
  --destination .af/catalog/optimizer --json
```

Review the lock, add it to `.af/task-catalog.toml`, retain the project's admitted `codex`
Provider principal/model policy, and commit that authority before running
`af self optimize --strategy light`. Catalog capture computes digests from committed bytes; do
not hand-type them. The model manifests request the captured `codex`, `gpt-5.6-sol`, `high`
preset. If that preset is not admitted, change and review the package before catalog capture
rather than substituting it at dispatch.

Live paid token/time demonstrations, savings claims based on those demonstrations and
longitudinal adoption observations remain pending milestone release gates.

## M3 implementation evidence map

| Requirement | Producer/consumer code | Runnable evidence |
|---|---|---|
| Actual light command path | `af/src/self_optimizer.rs` selects the light package only for a candidate-kind catalog with captured policy; `review-pipeline/src/task/optimization.rs` profiles and validates the generated proposal before existing M2 preparation | `af/tests/self_optimizer.rs::light_strategy_generates_one_candidate_without_exposing_source_to_author_workers` runs command, signed decision, protected arms, evaluator and replay |
| Least-context diagnosis/proposal and production packages | `OptimizationProfile` emits aggregate development evidence; `writable_configuration_view` exposes bounded policy-writable bytes; `OptimizationEnvironment` uses an empty sandbox without a source port; `optimization-light-model` selects captured-instruction model Workers | The light fixture rejects any author `source` input; `review-config/tests/optimization_package.rs` validates production model manifests and Pipeline |
| Executable recipes and unsupported capabilities | `optimization_light.rs` closes recipe fields; `light_recipe_catalog` installs context/cache hooks and records explicit upstream work; `prepare_light_configuration_with_proposal` enforces observations, effects, validation and invalidation. Artifact reuse is not installed without a consumer. | Core recipe tests, binding rejection, unsupported-recipe refusal, and the full command fixture's actual cache path |
| Protected candidate execution equivalence | Context preparation publishes one exact derivation blueprint; `OptimizationExperimentCoordinator` derives the candidate `TaskPackage@1`, selects it in the candidate definition and signed closure, and removes the blueprint from business inputs. `CapturedTaskHost` loads only that approved package and binds the context prepared from the exact registered definition to the reserved Attempt. `review-store` proves the package differs from the original only at `instructions.md`, with exact source/candidate/repin/package linkage. | `review-pipeline/tests/task_runtime.rs::approved_derived_model_child_uses_its_exact_context_and_replays_without_reexecution` captures the real model-adapter request for baseline, derived candidate and a later ordinary package invocation; it checks old-only/new-only instructions, selected package identity and absence of an instruction data copy. `substituted_resolved_context_is_rejected_before_model_dispatch` checks Attempt admission. `review-store` tests `child_plan_accepts_only_the_exact_instruction_derivation` and `child_plan_rejects_stale_linkage_and_every_non_instruction_change` cover original-package equality, unapproved derived signatures, stale configuration/source/candidate/repin linkage and changed manifest, model, effects and contract bytes. |
| Safe cache preparation and ordinary reuse | `OptimizationEnvironment`, now shared by every code Task profile, reads only the exact captured `.af/cache/cargo.json` contract, resolves only `CacheKind::Cargo` through machine policy, calls `materialize_cache`, supplies offline sandbox-local variables to non-writing command Workers, records `TaskRuntimeEvidence@1`, and removes `.af-cache` before sealing. Cache recipes require repeated latency comparison authority. `ExperimentTrialV1` admits the explicit `cache_toolchain_identity` missing field so the generic missing-measurement gate can withhold an unknown-toolchain result. | `af/tests/self_optimizer.rs::light_strategy_generates_one_candidate_without_exposing_source_to_author_workers` runs repeated cold/warm trials with measured timestamps on deterministic four-millisecond fixture boundaries, delivers and records adoption of the exact cache selection, and runs an ordinary code Task whose evaluator observes offline sandbox-local `CARGO_HOME` and the admitted bytes; the sealed Snapshot and worktree contain no `.af-cache`. The same test removes the captured toolchain, observes `unsupported_measurements`, and proves delivery refusal. `optimization_configuration::tests::ordinary_command_tasks_consume_only_the_exact_admitted_cache_selection` covers absent, malformed and unapproved selections; `cache_identity_changes_with_source_toolchain_policy_and_cache_content` covers invalidation; `review-sandbox/tests/cache.rs` covers safety and limits. |
| Development and holdout quality | `compare_experiment` checks every membership/family, not only holdouts | `review-core/tests/optimization_experiment.rs::broad_comparison_rejects_a_development_family_regression_even_when_holdouts_pass` |
| Economics decision and delivery gate | Finalization emits a preliminary recommendation only. After every Attempt settles, result assembly normalizes arm totals by matched trial units, refuses lossy signed arithmetic, requires complete native billing and timestamps, unions overlapping Attempt intervals, and replaces the preliminary value with an exact result. A non-integral measured per-run value is retained as `exact_per_run_normalization` and withholds adoption instead of rounding or failing the Task. Comparable workload, recurring estimates, one-off ceilings and correctness/latency exceptions come from captured project policy; proposal values cannot authorize delivery. | `review-core/tests/optimization_light.rs::economics_separates_gross_recurring_one_off_and_each_break_even`, `economics_normalizes_matched_trials_and_refuses_lossy_boundaries`, and the full command fixture's positive and withheld paths |
| Adoption identity and later Task projection | Local delivery emits `OptimizationAdoptionReceipt@1`. `af task observe-adoption` records Equivalent/Edited and attested labels. Optional `--evidence-task TASK_ID` ingests immutable common-Task revision/result/plan, successful and unsuccessful Attempt identities, usage/runtime receipts, bindings, engine/environment and named missing fields as non-causal `OptimizationAdoptionTaskEvidence@1`; `af task show` projects it. | `af/tests/self_optimizer.rs::light_strategy_generates_one_candidate_without_exposing_source_to_author_workers` observes exact and edited instruction-package adoption, observes exact cache adoption, runs the later ordinary cache-consuming Task and replays unchanged evidence idempotently. Core and schema tests reject causal or malformed evidence. |
| Closed Review findings | Economics production/gating, candidate-binding refusal, enforced recipe declarations, token-only ordinary payback, adoption receipt/observation production, the shared 64-edit bound, legacy request-digest preservation and schema/Rust control-character parity are mapped above or in the named core/schema tests. | `af/tests/self_optimizer.rs`, `review-core/tests/optimization_light.rs`, `review-core/tests/schema_parity.rs`, and `review-store/src/store/task/delivery.rs` |
| M1 accounting preservation | Light consumes `OptimizationHistory@1`; children, failed arms, optimizer overhead and later Task inspection remain native common-runtime records | Controlled-experiment and code-Task round-trip tests; adoption evidence retains missing usage/runtime fields instead of fabricating zeroes |

Remaining release evidence is explicit: live token-saving and time-saving project demonstrations
and later real-project adoption observations have not been performed. Credential-free fixtures
exercise the command and identity boundaries without claiming those live economics gates.

## Bounded candidate experiment checkpoint

Projects that pin an `optimization_candidate` kind package, the
`operator/optimization-experiment` Pipeline, and distinct baseline/candidate Workers may
request the M2 checkpoint explicitly. Existing projects and invocations remain on M1 unless
`--experiment` is present.

```console
af self optimize --experiment --execute --json > prepared.json
TASK_ID=$(jq -r .task_id prepared.json)
af task decision-payload "$TASK_ID" --developer owner --decision approved \
  --reason bounded-experiment-reviewed --output experiment.payload
minisign -S -s owner.key -m experiment.payload -x experiment.minisig
af task approve "$TASK_ID" --payload experiment.payload \
  --signature experiment.minisig --json
af task run "$TASK_ID" --execute --json > comparison.json
af task output "$TASK_ID" --port comparison --format json
```

The first command captures source, Requirements and history, compiles both declared
packages, prepares the exact child closure, and stops at `needs_plan_review` with zero child
Attempts. Approval verifies the detached signature against the Task's captured developer policy,
then atomically registers the same closure. The resumed process executes both arms as protected
verifier operations through the common scheduler, with each verification Attempt reserved inside
the original Task's verification limits. `ExperimentComparison@1` is reduced from registered
selected outputs, settled charges, and Store-recorded start/settlement times. Arm payloads cannot
supply `verified`, `protected_checks_passed`, or `billing_complete` fields, and an ordinary
successful Worker operation cannot stand in for the protected verifier class.
If an authenticated approved closure cannot be registered under the remaining parent limits, AF
retains the decision, moves the Task to `needs_human`, and dispatches zero child Attempts.

A signed rejection is recorded without registration or dispatch:

```console
af task decision-payload "$TASK_ID" --developer owner --decision rejected \
  --reason candidate-authority-rejected --output reject.payload
minisign -S -s owner.key -m reject.payload -x reject.minisig
af task reject "$TASK_ID" --payload reject.payload --signature reject.minisig --json
```

The `--experiment` checkpoint returns `inconclusive` with
`comparison_ready_finalization_missing`. It is useful for inspecting the common child lifecycle.
For a deliverable candidate, use the controlled configuration Pipeline below.

### Historical native receipts

The `af` adapter reads the public `af/task-inspection@3` through `@11` receipts. Generation `@9`
adds measured Attempt walls and `af/TaskRuntimeEvidence@1` sidecars emitted by the common Task
runtime; generation `@10` additionally retains prepared, decided and registered experimental
closures. Generation `@11` projects later immutable Task evidence beside an adoption observation
without treating it as causal proof. Experimental child Attempts remain ordinary execution records, so cumulative
usage-observed and settled charges are reconciled by Attempt identity and each baseline,
candidate, failed arm, retry and admission is counted once. Shared Code Task and captured Review
checks retain host-observed check spans. Captured
Review cache setup retains lookup/materialization timing, source digest and bytes made available.
That cache evidence is labelled `dependency_preparation`: it is never reported as a compiler or
provider cache hit, and absent toolchain or internal-hit evidence remains unknown.
Queue intervals come from exact reservation/start transitions; approval and user-wait intervals
come only from recorded planning/wait/resume transitions. AF does not infer provider-internal
phases from those host clocks.
A declared source must name the exact `execution_id` from `task_id`. Older receipts
do not attest a project, so the source must explicitly set `attest_project = true`;
reports retain `project_identity_attested` as a provenance limitation. A receipt
with a conflicting project or Task identity is refused. Cumulative usage updates
are merged per Attempt and reconciled against the receipt total, never summed as
independent charges. Components absent from the receipt remain unknown.

A declared provider source may set an absolute `project_root` to the original
project location when analysis runs in a separate checkout. Native cwd metadata
must match that declared root. This grants read-only source attribution, not
execution authority.

### Incremental source integrity and overlapping exports

Before continuing a cursor, capture rechecks the hashes of retained byte ranges,
including ranges preceding an empty capture. These reads consume the configured
raw-read allowance. A rewritten/truncated source or changed declared execution
identity produces an explicit unavailable-source gap; it is not silently treated
as an unchanged prefix. Import rotated content under an explicit new source
identity. Missing retained artifacts and branched capture heads are refused rather
than silently rewinding the analysis.

Native execution observations are deduplicated across exported source names while
all source receipts remain retained. Historical native observation IDs remain
readable and do not cause duplicate failure counts on recapture. Normalized fixture
records retain their occurrence/range identity. A complete zero-based capture clears
a partial-history warning only for the source/execution ranges it actually covers;
another incomplete source using the same adapter remains visible.

Delivery requires protected verifier evidence from registered children and an independent
non-child evaluator, selected and published by the common runtime. A command arm's successful
exit alone is not that evidence. `af/ExperimentTrialResult@1` retains protected measurements;
charge and billing completeness are derived from the common Attempt ledger. Signed rejections
remain factual decisions and grant no registration or dispatch authority.

### Controlled configuration candidates

Install the project-owned `optimization-controlled`, `optimization-check-baseline`,
`optimization-check-candidate`, and `optimization-check-evaluator` packages from
`fixtures/self-optimizer/catalog` alongside the candidate Task kind. Compute their catalog digests
with AF's package tooling and commit the authority before execution. The independent evaluator
has its own protected Verify slot; the baseline and candidate are separately approved child slots.

Declare a captured `.af/optimization-policy.json`, for example:

```json
{
  "schema": "af.optimization-configuration-policy/1",
  "writable_paths": ["harness.py"],
  "checks": {"oracle": "oracle.py"},
  "harness_path": "harness.py",
  "experiment": {
    "recipe": "deterministic_correction",
    "uncertainty_rule": "deterministic",
    "repetitions": 2,
    "minimum_families": 2,
    "token_increase_ceiling_bps": 0,
    "cases": [
      {"family": "syntax", "membership": "holdout", "input_path": "fixtures/syntax-case.json"},
      {"family": "behavior", "membership": "holdout", "input_path": "fixtures/behavior-case.json"}
    ]
  }
}
```

Every captured file outside the writable roots is protected, including the policy and oracle
scripts. The supplied command packages execute those Python checks in each isolated fixture.
A proposal supplies bounded text edits, never authority or a replacement verdict:

```json
{"schema":"af.optimization-candidate/1","edits":{"harness.py":{"text":"print('correct')\n","executable":false}}}
```

Run `af self optimize --candidate candidate.json --execute`, review/sign the exact prepared
closure using the approval commands above, then resume with `af task run`. The installed prepare
operation validates the diff, constructs baseline/candidate fixtures, and completes package
repinning before any evaluation. Harness-only edits retain an explicit unchanged-lock receipt.
The finalizer requires the accepted comparison and independent evaluation for the exact final
Snapshot and Requirements. Only a satisfied result can be delivered through `af task deliver`.

The captured policy freezes the recipe, case-family membership, repetitions, minimum-family
threshold and token ceiling before either arm runs. Families already disclosed by imported
history cannot be reused as fresh holdout evidence. Each case and repetition expands to matched
baseline/candidate children under the original Task budget. The exact captured Worker bindings
may be bounded command or model Workers; package identity, model, effort, effects and per-Attempt
bounds are copied into the signed closure. Measurement Workers remain effect-free.

The reducer derives charges, retries, failures and execution time from the Store ledger. It also
consumes shared `af/TaskRuntimeEvidence@1` preparation and cache receipts. Missing preparation or
cache instrumentation is retained explicitly and prevents token or latency savings acceptance;
it is never treated as zero overhead. Deterministic correction keeps its separate protected
baseline-fails/candidate-passes rule and does not establish a broad performance-savings claim.

Each declared case names a protected captured JSON input file. The installed preparation
operator retains its canonical identity and binds an `af/OptimizationCase@1` input to each arm.
Different family labels cannot relabel identical JSON data to satisfy a minimum sample count.
Measured Worker packages must explicitly declare and consume that case input. The supplied
check Workers pass its protected path as the check script's first argument; the independent
evaluator reruns every declared case against the final candidate. Family independence remains
a property of the predeclared evaluation design, not a claim established by different labels.
