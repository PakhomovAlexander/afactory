# Performance Scenarios: manual proof, then a pinned Ant

The native bridge submits durable experiments to the existing Performance Executor. It creates no
benchmark runner, arbitrary command grant or scheduler. Remote publication is a separate opt-in
workspace capability described below. GitHub Actions remains
a separate executor backend. This bridge requires `backend = "local"` and runs on Claw, after the
implementation Task, local native gate, both reviewers and evaluator have passed. Nothing in this
runbook has been installed or live-tested by a Worker.

## Authority and exact sources

An `experiment_submit_v1` Action requires its own exact operation/destination grant in the Scenario
and machine policy. It is neither `observe` nor `notify`. Its typed inputs are:

- `candidate`: an accepted native Task receipt, never a supplied root receipt;
- `subject`: that receipt's `result_id`, projected with `task_result_id`;
- `revision`: its exact `snapshot` output Snapshot, projected with `task_output_snapshot`;
- `body`: `af.perf-scenario-experiment/1`, declaring `work`, `configuration_digest` and
  `producer_digest`. The closed work set is `core` or diagnostic `telemetry`.

The adapter commits the intent, exact input digest and native source request before returning a
submission receipt. It launches no compute and does no heavyweight preflight in that call. The
separate token-free submission service invokes `af scenario experiment-source`. That native command
reopens the fixed machine Task Store, checks the exact revision, plan, result and acceptance, and
checks the candidate and original source Snapshots. It uses the existing Manifest/path encoding and
filter-free Git materialization helpers, never `git add`, candidate attributes or hooks. Private
deterministic commits are benchmark representations, not commits in the developer's repository.
The destination is outside every repository and the Task Store, under an owned parent unwritable
by other accounts. No checkout, user branch or remote changes. The receipt retains both original Snapshot IDs,
content digests, origin IDs and repository identity beside the private commit IDs. The handoff
request digest is the adapter protocol's SHA-256 of canonical JSON; Snapshot and artifact IDs
retain their separate native CAS domains. The Python/native bridge checks these identities.

`af/scenario-experiment-source-request@1` optionally names `baseline_snapshot_id`; null or absent
means the native Task's original source, never main. A supplied different source refuses. The
candidate must always match. Captures refuse links, submodules, more than 32768 entries per side
or more than 1 GiB per side or installed control. The adapter reserves 12 GiB for source objects,
temporary raw blobs, Git transfer and metadata under both its sub-budget and the native journal's
capacity/free-space admission. A torn capture refuses on restart; no automatic deletion or
regeneration hides it.
Replay checks exact receipts, recomputes native trees and runs Git object integrity verification.
New captures require the installed control to be the exact reviewed main in the configured Task
repository; the private trusted ref is frozen to it. Replay uses that retained authority even
after main advances. Missing or unsupported control refuses rather than substituting main.

Submission then uses `Journal.submit`, including its native preflight, installed producer, frozen
configuration, immutable original, archive and native verifiers. Its configuration differs from
the pinned operator configuration only in the private source repository path; both identities are
retained. A changed producer refuses before a native original is reserved. There is no second
benchmark executor or alternate coordination lock. Native execution still takes
`/tmp/af-perf-machine.lock` and its anchored inode and fence. These services never remove or
recreate that lock or clear an uncertain fence.

## Recovery and completion

The submission service processes at most one pending intent per invocation. Lost responses recover
the same intent and native submission key; a claim is never replayed. Common lock contention leaves
work queued. Journal contention returns busy. Definitive pre-submission refusals retain a typed
failure; a partially published original remains uncertain. A torn receipt or changed configuration
fails closed. Every intent and negative result stays inspectable in the private spool and journal;
capacity refuses new work rather than evicting them.

A separate `native_verified` Wait uses the Action's `action_intent` projection as `subject` and the
same candidate Snapshot as `revision`. Each observation is bounded and opens no model session.
A later fresh Task must bind an accepted `source` predecessor with that exact Snapshot provenance
and unchanged content; native re-rooting is followed, while unrelated or changed trees refuse.
The token-free verification service admits at most one new certificate per invocation through the
native offline verifier. Its completion certificate is bound to the exact native
request, producer, sources, frozen configuration and result digest. It carries the native paired
verdict and the original Snapshot identities into later Tasks. Process success and HTML filenames
are insufficient. Failed execution, incomplete reports, synthetic timing, stale result receipts
and changed sources cannot satisfy the Wait. A measured regression remains valid negative
evidence, never an improvement. Telemetry remains diagnostic, even when its native run completes.
Reports larger than the 24 KiB evidence allowance refuse explicitly; narrow the reviewed workload.

Cancellation stops future Scenario dispatch and polling, retaining all effects and accounting.
An experiment already submitted belongs to the native executor and may still be queued or running;
Scenario cancellation does not claim its rollback. Inspect its original and, if needed, stop that
specific executor service through its existing cancellation/cleanup procedure. Never resubmit a
claimed or uncertain original. Confirm native cleanup before any operator fence recovery, following
[the executor runbook](performance-local-executor.md). Unknown Task charges keep their unused loop
reservation across restarts, dates and roadmap revisions.

## Bounded research evidence

`scripts/performance/scenario.py evidence` rebuilds a history index from explicitly selected native
original IDs and captured native findings, Optimization History Captures and adoption observations.
It does not scan an archive into a prompt. Supply `--sources` as a JSON array of objects with
`task_id`, exact native `artifact_id`, `observed_unix_ms` and `compatibility`. Also supply
`--af`, its `--af-digest`, `--task-repo` and `--task-state`. The trusted native
`af scenario evidence-source` reader verifies each envelope against the Task Store, reconstructs
findings from their recorded source round, checks History Capture input receipts and checks
adoption against its recorded observation and delivered receipt. Arbitrary JSON exports, even
with correct file hashes, are refused. Select at most 256 native artifacts (4 MiB each) and 256
native original IDs. Completed native originals are independently verified. Failed, uncertain,
inconclusive and already adopted work stays in the index. Adoption records attest local delivery
and observed commit equivalence; they do not independently prove a remote merge. The index is a
disposable projection; deleting it does not reset native history, receipts or accounting.
Rebuild it from the same selected authoritative artifacts. For a findings capture, select the
continuation Task that records `continues.findings_id` in `af task show --json`, rather than
an exported `af task findings` file. History selections name a recorded native `history` input;
adoption IDs are listed under `adoption_observations` in the optimization Task's inspection.

Packets are immutable content-addressed documents carrying source IDs/digests, index digest,
cutoff, creation time, compatibility, age bound, item/byte limits and counted omissions. `compatibility_basis` distinguishes the native request-derived workload
identity from an owner-declared selection label; matching the latter is not independently
verified timing compatibility. `fresh`
means fresh **at packet creation**, not at an arbitrary future read. Incompatible and stale evidence
is retained and labelled. Provenance distinguishes a verified native result, native journal state
and a native Task artifact receipt; freshness distinguishes native terminal/cutoff time from the
owner's capture observation time. Capturing an old finding today does not establish when it was
first reported. A packet covers its explicit selection, not every artifact in an archive.
Each finding is a separate item carrying bounded title, location, evidence and proposed fix,
with explicit truncation and native Task/revision/artifact provenance. Byte and item limits count
omissions. Raw telemetry, logs, environment and credentials are excluded. Counts
are not causal evidence or a speedup claim. Native matched timing is the improvement authority. Span maxima remain distinct from total wall
time, and non-exact spans stay explicitly excluded.

The adapter's `bounded_evidence` Wait selects the pinned index digest as `subject` and the wanted
compatibility digest as `revision`. Its installed policy caps packets at 24 KiB, 256 items and an
explicit age. Before satisfying the Wait, the adapter rederives every selected record through
native Task Store artifacts or journal originals, using offline verification for completed
experiments. All bounded facts must match the index; a pinned JSON digest alone grants no
native provenance. Original lookup uses the journal's configured membership bound, and source
retrieval retains the 256-artifact and 4 MiB per-artifact bounds. `scenario_evidence` is an optional, typed Observation input on fresh Task steps.
Only satisfied `bounded_evidence` or `native_verified` observations enter that reserved field of
the Task's immutable Requirements. Templates cannot prefill it or `scenario_goal`. Research
receives a bounded packet; verification and reporting receive the exact native completion.
The candidate and final report also bind the accepted research Document on `research_report`.
Its exact native receipt, acceptance and original source are verified before dispatch, and its
envelope is bounded to 32 KiB. Both Workers receive the same artifact, including its useful
recommendation; ordering alone never passes data. The owner pipeline must declare and forward
this optional `af/Document@1` input, as the shipped implementation-reviewed/report contracts do.

Example authoring command (operator-selected paths and digests):

```sh
python3 -I scripts/performance/scenario.py evidence --sources /private/perf/sources.json \
  --af /opt/af/bin/af --af-digest sha256:EXACT_AF_BYTES \
  --task-repo /project --task-state /private/tasks \
  --executor /etc/af-perf/executor.toml --original exp_EXACT_ID \
  --output-root /private/perf/evidence --compatibility sha256:REVIEWED_COMPATIBILITY \
  --cutoff 1793664000000 --created 1793664000000 --max-age-ms 86400000
```

Use the emitted index path/digest in the owner-only adapter configuration. The packet and index
schemas are generated with their readers by `scripts/performance/schemas.py`.

## Review and prepare the inert bundle

The examples are `fixtures/scenarios/performance-candidate.json`, `performance-no-worthwhile.json`,
`performance-native-roadmap.json`, `performance-policy.json` and `performance-ant.json`. Their
paths, digests and authority commits are placeholders. The candidate bundle owns one Store replay
goal (`state_projection`, paired median `wall_ms` on `events_10000`): bounded research, one small
code-correctness candidate, native comparison, independent read-only verification
and an independently verified final report. The no-worthwhile variant instead requires an accepted
research report proving that branch with evidence and costs; it skips candidate/experiment work
and returns that report. It fails acceptance when that claim cannot be established. Select the
variant explicitly when reviewing the pin; no boolean supplied by a model grants completion.

The committed `.af/task-packages/owner-default/` Workers supply the exact owner development
team below. Each reuses the current kernel public contracts and Attempt bounds. The development
instructions follow the actual performance Task goal and requirements, without assuming a
campaign package plan or an unconditional ADR deliverable. The implementation Worker accepts
the optional typed
`research_report` Document in its signature and input schema, retains it on both outputs and
instructs the Worker to consume its recommendation. The generic Workers, defaults and Pipelines
remain unchanged. Use the existing local `kernel/implementation-reviewed`,
`kernel/verification-reviewed` and `kernel/report` Pipelines, with these inert slot/provider-only
bindings:

| Stage | Bindings file | Effective Workers |
| --- | --- | --- |
| Candidate | `fixtures/scenarios/performance-implementation-bindings.json` | Four `owner-default/` development Workers |
| Verification | `fixtures/scenarios/performance-verification-bindings.json` | Three `owner-default/` judges; no implementer |
| Research and final report | `fixtures/scenarios/performance-report-bindings.json` | `owner-default/report-author`, `kernel/report-verifier` |

These files contain no local package definitions, authentication paths or credentials. They do
not activate a Task, Scenario, Ant or publication. All selected Workers use `codex-personal`;
that alias must already name the operator's admitted Codex account on the capture machine.
The Report author uses Sol medium with only `read-source`, the existing Report contract and
unchanged author reservations. The existing independent Report verifier uses Sol high with
only `read-source`. The generic `kernel/analyst` uses Claude and cannot be switched to Codex by
a Provider alias alone; the separate committed owner author makes this selection feasible.

For a consuming repository, copy the complete pinned package/Pipeline closure into its reviewed
catalog authority. Afactory's catalog review policy keeps `bugs = "advisory"` and
`correctness = "required"`; these bindings do not change that policy or its benchmark-Demand
reduction. The selected production `bugs`, `correctness` and `evaluator` slots and results are
mandatory under native performance role and publication checks. That complete team obligation
does not rewrite the catalog's reviewer requirement. Inspect the complete compiled plans. New package pins are generated with `af catalog digest`, never
hand-typed. Current code check contracts use `af/TaskCheckReceipt@2`; mutable local package
overrides and silent old-role fallback are refused by native performance template admission.

| Development role | Model | Effort | Provider |
| --- | --- | --- | --- |
| Implementer | `gpt-6.1-sol` | medium | `codex-personal` |
| Bugs | `gpt-6.1-terra` | high | `codex-personal` |
| Correctness | `gpt-6.1-terra` | high | `codex-personal` |
| Evaluator | `gpt-6.1-terra` | medium | `codex-personal` |

All owner Workers use GPT-6.1: Sol for implementation and Report authoring, Terra for the
development judges, preserving their model independence from the Sol implementer.
There is no advisory role or default-light pipeline. Verification has no implementer. Native
performance roadmap admission checks these model bindings before dispatch. Research and final
reporting use the explicit read-only Report bindings above.

The example allowances account for the shipped package reservations, including bounded retries and
Provider admission; they are authorization bounds, not expected spend:

| Stage | Task tokens | Task wall |
| --- | ---: | ---: |
| Research and its independent report verification | 5,000,000 | 24 hours |
| Small candidate, both reviewers and evaluator | 8,000,000 | 48 hours |
| Independent read-only verification | 5,000,000 | 24 hours |
| Final report and its independent verification | 5,000,000 | 24 hours |

The verification template protects 12 hours of its 24-hour Task wall for verification;
the remainder permits native Provider admission before judging begins. A reserve equal to the
whole Task wall refuses performance planning, since it leaves no dispatchable admission window.
This changes no Worker Attempt wall or Task token allowance.

The declared Task sum is 23,000,000 tokens; a goal reserves 32,000,000. Each Scenario step permits
one Task attempt; the Task plan retains its existing per-Worker retry bounds. The native comparison
needs no tokens and has its own declared executor bounds. Eight elapsed days cover the five Task
days, bounded history/report waits and the native executor's at-most-eight-hour original. Per-run
and UTC daily allowances are 32,000,000; seven-day rolling and roadmap-total bounds are 96,000,000.
Active/unknown reservations remain held regardless of age. Smaller production limits require reviewed smaller Worker packages,
new native plans and a new Scenario pin; the illustrative 10,000-token admission fixture is not a
production model-loop budget. Inspect native plan reservations and refuse if any no longer fit.

Export each stage's `operation.template.document` to an external Task file. For example, from
the reviewed repository, capture the candidate using its committed owner packages:

```sh
af scenario template-pin --file /private/perf/candidate.json --repo /project \
  --task-state /private/perf/tasks --authority-commit EXACT_COMMITTED_AUTHORITY \
  --bindings fixtures/scenarios/performance-implementation-bindings.json --json
```

Use `performance-verification-bindings.json` for verification and
`performance-report-bindings.json` for both research and final reporting in the same command.
The authority commit must already contain these packages and their generated catalog pins;
an unmerged authority-only commit cannot satisfy standing publication's remote-main equality.
Capture and planning do not merge, install, activate or publish anything. Preserve each stage's
goal, mode, explicit limits and `facts.standard = true` on the candidate and
verification templates. Token-free native planning must succeed for all four stages before the
operator approves execution. Template capture and planning run no Provider inference; planning retains token-free CLI
installation, capability and account-identity checks. Collect the four returned
envelopes under `research`, `candidate`, `verification` and `report` in `pins.json`. Capture the
exact committed owner authority; review model bindings and contracts before any execution.

Create/review the native roadmap and record its exact `roadmap`, opaque `identity` and `revision`
in `admission.json`. Native admission still reserves the complete loop atomically. Failed goals
and unknown usage do not become fresh allowances under another date or goal ID.

Configure `af.perf-scenario-adapter/1` using its
[shipped example](../fixtures/performance/schema/scenario-adapter/valid.json) and generated schema.
All paths are fixed machine
paths: private spool, native executor configuration, installed native `af` plus byte digest, Task
repository/Store and evidence root/index/digest. Spool and evidence roots are disjoint private
children of the existing native journal root, outside repositories, and count under its capacity. The local
allowance is a sub-budget, not another machine storage allowance. Source capture also holds the
native common lock and fence. Use the same authorized account for the private
Task Store, adapter and executor; do not relax private Store permissions to bridge accounts.
Obtain the installed producer and executor configuration digests from native preflight with the
installed control. Review that preflight; it reserves no experiment and runs no benchmark.

Generate a launcher **on the operator machine**, outside a checkout:

```sh
python3 -I scripts/performance/scenario.py launcher --config /etc/af-perf/scenario.json \
  --output /opt/af-perf/scenario-adapter > /private/perf/launcher.json
```

The launcher pins its entire Python module closure and configuration before importing them. Pin
the exact generated executable digest in machine Scenario policy. Changed modules, configuration
or native binary bytes refuse and require fresh review/pins. Install no credential in a definition.

Render the external authoring files from native receipts:

```sh
python3 fixtures/scenarios/configure-performance.py \
  --bundle fixtures/scenarios/performance-candidate.json --pins /private/perf/pins.json \
  --launcher /private/perf/launcher.json --configuration sha256:EXACT_CONFIGURATION \
  --producer sha256:EXACT_PRODUCER --evidence-index sha256:EXACT_INDEX \
  --compatibility sha256:EXACT_COMPATIBILITY --admission /private/perf/admission.json \
  --output /private/perf/manual
```

This only writes inert files. Inspect them, update the Ant owner/date/timezone/routing label, and
review native Scenario/Ant validation and every Task plan. The formatter never confirms or runs
them. A new evidence selection also requires a reviewed launcher and Scenario pin.

## Manual proof and distinct Ant enablement

1. After native implementation/verification pass, install reviewed code and native executor
   authority on Claw as the operator. Keep the existing native executor service and common lock
   anchor/fence. Review the inert `af-perf-scenario.service.in`/timer examples before installing
   them; submission and offline verification are separate from the existing compute service.
2. Run `af scenario validate --file /private/perf/manual/bundle.json --authority
   /private/perf/manual/policy.json --json`. Inspect its complete pin and resource/authority
   closure. Start exactly that pin with `af scenario start performance-manual --file ...
   --authority ... --inputs /private/perf/manual/inputs.json --confirm-pin REVISION --execute`.
3. At each bounded Wait inspect the durable Run, intent, native original, source receipt and
   report. Drive `scenario.py submit` and the ordinary native executor service, then
   `scenario.py verify`. Resume the same Run; no model is held during experiment waits. Retain
   native report evidence, both reviewers, evaluator, independent verification, final accepted
   report and the roadmap/Run completion. Demonstrate restart reconciliation on those receipts.
4. Only after one **real complete manual candidate loop** succeeds, review the exact Ant pin:
   `af ant validate --file /private/perf/manual/ant.json --authority
   /private/perf/manual/policy.json --json`. Add it with `--confirm-pin REVISION`; add registers
   it disabled. The operator's separate `af ant enable performance-native` authorizes future
   occurrences. Review the inert `af-performance-ant.service.in`/timer before Linux systemd
   installation. No Mac scheduler is supplied. Overlap and missed occurrences skip; no catch-up
   burst or duplicate complete goal is launched. A completed manual goal is not recycled: this
   one-goal example subsequently skips roadmap admission until the owner reviews genuinely new
   eligible goal scope and updates the pin.
5. Ant enablement authorizes neither a PR nor a merge. Publication still requires a separately
   granted publication Action. Generic `publish_pr` requires an exact signed human Decision for
   that Run/pin/object/revision. The optional performance operation below uses scoped standing owner
   authority. The default examples carry no publication grant, credential or Decision key.

Live Claw proof, installation and Ant enablement are operator acceptance, not Worker work. Do not
mark them complete from hermetic fake adapter results or a successful local Task gate.

The binding integration decision is
[ADR-2026-11-03-native-performance-scenario-loops](adr/2026-11-03-native-performance-scenario-loops.md).
Evidence index and packet documents use generation 2; old generation 1 evidence fails closed.

## Opt-in standing performance PR publication

The separate `publish_performance_pr_v1` operation implements the owner's scoped standing
publication authorization in
[the decision](adr/2026-11-06-publish-verified-performance-under-standing-owner-authority.md).
Generic `publish_pr` and `followup` still require their exact signed Decisions. Human merge is
required. Task delivery and Remote Checks retain their existing authority.

Prepare the installed [workspace/GitHub adapter](scenario-git-workspace.md) outside every checkout.
Start from the inert `fixtures/scenarios/performance-workspace.example.json`.
Its owner-only `af/git-workspace-config@2` pins the exact repository numeric identity, native
repository and Task Store, reviewed source commit/content, base, branch namespace, native tools,
SSH config and author. Set `operations` to `["publish_performance_pr_v1"]` and add:

```json
{
  "performance": {
    "executable": "/replace/private/native-lab-adapter",
    "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
    "destination": "native:claw"
  }
}
```

This is an inert fragment, not a complete configuration. Use the exact lab launcher receipt from
this runbook and populate the other fields from native reviewed receipts. Install only after
reviewing the complete config, credentials and exclusive writer scope:

```sh
python3 -IS scripts/install-scenario-git-workspace.py \
  --config /replace/private/performance-workspace.json \
  --output /replace/private/performance-workspace
```

The installer prints the executable digest. Copy its path/digest into a private copy of
`fixtures/scenarios/performance-publication.example.json`. Its `scope` repeats the exact installed
repository name and numeric ID, native repository/Task Store, source commit/content, base and
branch prefix. Keep `evidence_adapter: lab` and `evidence_destination: native:claw`. This file
contains no signing key. Validate it against `af/performance-publication-activation@1` and review
all values before supplying it to the authoring command:

```text
# Add to the configure-performance.py command above, for the candidate bundle only:
 --publication /replace/private/reviewed-performance-publication.json
```

The generated bundle adds a final typed Action after implementation, native comparison,
independent verification and accepted final reporting. Its inputs are `candidate`, `verification`
and `report` native Task receipts, `measurement` from `native_verified`, the candidate result as
`subject`, its exact Snapshot as `revision`, and `body` with `af/performance-pr-content@1` and a
single `title`. The body of the PR is generated from native records. The machine policy contains
an explicit `performance_publications` scope keyed by the installed publication adapter. The
native runtime injects that scope into the adapter request; Scenario/model text cannot supply it.
The generated Ant remains inert until the existing separately confirmed enablement procedure.
The no-worthwhile bundle has no candidate publication step; asking to add one refuses authoring.

Before any candidate commit, push or POST, native acceptance/provenance is checked again and the
pinned lab adapter re-observes its retained offline-verified evidence. The independent verification
must reference the implementation's exact accepted result and leave its Snapshot unchanged. The
accepted final report must reference that verification and consume this exact native comparison.
Local and remote base must equal the installed source commit. A `.github/` change refuses.
The existing workspace export limit remains 32768 entries and 64 MiB per candidate; unsupported
sources refuse before commit.
Only passing, complete, measured local core comparisons with at least one non-exploratory native
improvement and no failing/inconclusive case qualify. Telemetry, regression, diagnostic, synthetic,
unsupported and within-tolerance-only evidence produces no PR. Local partial/bootstrap scope is
stated explicitly and never claims a remote required-check pass.

The default is a normal open PR. It carries the accepted report, the actual `af task report` block
for implementation, independent verification and final report, and the exact native comparison
including before/after medians, rational change and intervals, sources, producer/configuration and
scope. No Worker-supplied body or boolean replaces those receipts.

The operation creates a deterministic commit and only a new branch under the pinned namespace.
An empty expected-ref lease requires atomic remote absence; it grants no force-update of an
existing ref. Existing or foreign refs are retained. This operation has no continuation, merge or
cleanup permission. After a lost response, resume the same Run/intent: deterministic commit bytes
recover the same object, exact remote ref readback avoids a second push, and retained GitHub markers
recover the single PR. An uncertain POST with an empty listing stays uncertain and never repeats.
Do not delete the private journals, reuse an intent, widen the scope or start another Run to
resolve an ambiguous effect. Inspect retained receipts and the remote first.

`af scenario performance-proof --file REQUEST --repo REPO --task-state STORE --json` exposes the
read-only native checks and report rendering as `af/performance-pr-proof@1` from an
`af/performance-pr-proof-request@1`. It grants no publication authority by itself. The installed
adapter independently checks the retained lab evidence and machine scope before effects. Changed
Scope/Scenario and adapter contracts use their current versions; finish or discard older Runs
under their own release. No compatibility migration, live install, Mac remote Gate or benchmark
is part of this change.
