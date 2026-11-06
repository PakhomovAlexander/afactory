# Scenarios and Ants

A Scenario orchestrates pinned existing Tasks or authorized fresh Task templates, child Scenarios,
bounded actions, signed human decisions,
and observations. An Ant binds a pinned Scenario to a daily schedule. Neither command installs a
cron job, starts a daemon, or creates an instance until explicitly requested. Models run only
inside Tasks. [ADR-0274](adr/0274-orchestrate-pinned-scenarios-and-admit-ant-occurrences.md)
defines the authority boundary.

## Review, record, drive

Definitions use TOML for authoring and JSON for their versioned wire documents, with schema
`af/scenario@4`. A bundle (`af/scenario-bundle@4`) names its
`root` and maps names to definitions. See the inert examples in `fixtures/scenarios/` and the
schemas from `af schema show`. Definitions have explicit `inputs`, `outputs`, ordered `steps`,
`result` bindings, boolean `completion` conditions, `authority`, and `limits`.

The port types are `text`, `boolean`, `integer`, `task`, `action`, `decision`, `observation`, and
`owned_file`. A value is `{"type":"text","value":"example"}`. Bindings have `source` set to
`literal` (with `value`), `input` (with `port`), or `step` (with `step` and `port`). Forward
references and type mismatches fail admission. A `project` binding selects a typed receipt field:
`task_conclusion`, `task_result_id`, `task_output_id` or `task_output_snapshot` (the latter two
name a `port`), `action_intent`, `observation_satisfied`, or `decision_approved`. Its `value` is
another binding and its `projection` is an object with `kind`. An `equals` binding compares scalar
`left` and `right` bindings of the same type and returns a boolean. This allows a Task's evidenced
`no_worthwhile_optimization` conclusion to skip implementation and still complete successfully.
A `select` binding has boolean `condition` and equal-type `when_true`/`when_false` bindings;
it resolves only the chosen arm, so optional skipped outputs remain unread. Decision branches
are refused to keep signed approval consumers static. Root inputs and literals are scalar data; they
cannot manufacture receipts. Child inputs may carry receipts produced by preceding steps.

Each step declares `id`, `operation`, `inputs`, `outputs`, `when` (null or boolean binding),
`max_attempts` (1–8), `timeout_ms`, and `on_timeout` (`fail` or `skip`). Only Wait and Decision
may skip on timeout. A false condition records its identity and skips the step. Skips produce no
outputs; consuming a skipped port fails rather than inventing evidence. Completion requires
all declared boolean conditions to be true and every declared final output to resolve.

Review the full pin and its authority before confirming its exact `revision`:

```text
# needs: bundle.json, inputs.json, and owner-only machine policy ../scenario-policy.json
 af scenario validate --file bundle.json --authority ../scenario-policy.json --json
# needs: the exact revision just reviewed
 af scenario start example --file bundle.json --inputs inputs.json \
   --authority ../scenario-policy.json --confirm-pin sha256:... --execute --json
 af scenario show example --json
 af scenario resume example --json
 af scenario cancel example --reason 'operator stopped this work' --json
```

All namespaces take `--state /absolute/external/directory`, `--json`, and optional `--now N`
(Unix milliseconds). Normally the machine supplies time; `--now` supports deterministic external
scheduler invocation and testing. The caller must supply nondecreasing times. State belongs
outside the checkout and defaults to `$XDG_STATE_HOME/af/scenarios`. Its journal lives under
`runs/` in the ordinary private registered Store infrastructure, so the machine Storage Budget
counts its bytes as Store remainder. Durable records are retained with a 16 MiB document bound.
`scenario start --execute`, `scenario resume`, and `ant tick` install the machine Storage Budget
before execution and sweep it afterwards, including when the Run contains only orchestration
steps. The sweep enforces the byte budget; age-based collection also runs when `auto_gc` is on.
Each admitted Run receives an opaque execution identity, so equal labels in separate Stores
cannot share action idempotency keys or human decisions. The private SQLite journal
checks its version, release, checksum, transitive pins, signatures, accounting and progress when
read. Missing or corrupt state fails closed. `show` retains the full pinned definitions,
receipts, child frames, intents, waiting reason and final evidence. `list` enumerates records.
Successful command execution returns 0; Run status is data, including `waiting` and `failed`.
After startup, refusals return 1 with the common JSON error document when `--json` is given.
A Storage Budget setup failure returns 1 with a diagnostic on stderr before the command starts.

## Steps and recovery

- Task: `operation.kind: task` names an existing Task by absolute `repo`, `state`, `task_id`,
  exact `plan_id`, exact `revision_id`, and `max_tokens`. Machine policy separately permits the
  plan, and the Scenario declares it in `authority.task_plans`. Child plan authority must be a
  subset of the parent's, as must action grants and decision owners. It dispatches
  `af task run --confirm-plan` through the ordinary runtime. Generated plan
  signatures, sandboxing and Task acceptance still apply. A process exit is not evidence: the
  coordinator reads the accepted result, output artifacts and accounting from the Task Store.
- Scenario: `operation.kind: scenario` names a bundled definition. Its input and output ports
  must match exactly. Recursion, excessive depth, widened authority and larger child budgets
  fail admission. The Run pins the complete transitive bundle. Editing a definition changes
  future pins; it cannot change a Run on resume. Child frames share root accounting, and one
  Task revision in one Store is charged once even when consumed more than once.
- Decision: `operation.kind: decision` names an authorized owner. Its text inputs `subject` and
  `revision` identify the exact object. `scenario decision-payload RUN --out FILE` writes the
  canonical signing bytes, prefixed by `af/scenario-decision@1` and a newline. Sign FILE with
  minisign outside af. Submit an `af/scenario-signed-decision@1` JSON envelope with `schema`,
  `request`, and
  `signature` (the minisign signature text) through
  `scenario decide RUN --file SIGNED_JSON`. REQUEST is the JSON that decision-payload prints.
  `--reject` makes a rejection payload. The machine policy maps owners to public keys. Signing
  keys never enter af. The signature binds Run label, opaque execution identity, transitive pin,
  step, object, revision, choice,
  owner and deadline. A different head or expired decision is refused. Resume consumes it.
- Wait: `operation.kind: wait` names an `observe` grant and a `condition`. Inputs are exact
  `subject` and `revision`. One bounded adapter observation runs per resume; unsatisfied
  conditions return control in `waiting`. A changed observed revision fails as stale evidence.
  Waiting consumes elapsed time and remains active for overlap admission, with no model session.
- Action: `operation.kind: action` names an exact grant. External operations take text inputs
  `subject`, `revision`, and `body`. `requires_decision` adds a signed `decision` input;
  `required_condition` adds an `evidence` observation input with that exact condition. Both must
  match the same object and revision. Publication (`publish_pr`, `followup`) always requires a
  signed human decision. No Task delivery, commit, push, merge or blanket shell grant follows.
  The native `experiment_submit_v1` Action additionally takes an accepted `candidate` Task receipt;
  its separate verified-report Wait is described in the performance runbook.

A [structured Task example](../fixtures/custom-tasks/README.md) projects the exact typed output
of one captured issue Task through `task_output_id` and checks `structurally_validated`. This
uses the same pinned Task projection as every other kind, without report or evaluator nodes,
dynamic Task creation or publication authority.

A Decision receipt used for publication may authorize exactly that one decision-consuming Action
in the pinned bundle, including uses through aliased child inputs and outputs. Admission rejects
any second consumer that requires a Decision, including `notify`, even if it is conditional or
uses the same grant. Dispatch checks the producing Decision invocation;
the signed pin binds that consumer's exact step, adapter, operation and destination. Changing
any grant changes the pin and invalidates the approval. Use separate Decisions for separate
publication Actions; ordinary receipt projections and child forwarding remain available.

Limits bound tokens, elapsed time including waits, and dispatch count. Task allowances are reserved
before dispatch; terminal Tasks settle their exact recorded charge, including failed Tasks.
Uncertain work keeps its reservation. A reported overrun is retained and stops further dispatch.
A child cannot enlarge a parent's allowance. Retries are finite:
only a definitive failure can create another intent, and all prior intents remain inspectable.
A lost external response requires reconciliation of the same intent, never a blind retry.
Cancellation records its reason and typed child stop requests, bound to the exact Task run.
Resume or repeated cancel recovers a lost handoff without starting work or signalling a later
manual run of that Task. Confirmed requests do not signal again. Receipts and uncertain
intents remain; a completed side effect is never described as rolled back. Cancelling a Run and
pausing its Ant are separate operations.

## Machine policy and bounded adapters

`af/scenario-policy@4` is an explicit, owner-only machine file outside every Git checkout.
Author it as TOML; its captured JSON wire form is also accepted. The filename selects the format
(`.toml` or JSON); a parse failure never selects a fallback format or an earlier version.
It contains
`task_plans`, `task_templates`, `decision_keys`, `file_roots`, and `adapters`. The pin includes the
entire effective
policy. No workflow supplies executable paths, arguments, environment variables, or credentials.
Each adapter is an installed, trusted program outside the checkout, pinned by executable SHA-256,
with finite `operations`, exact `destinations`, `max_wall_ms` (at most 30 seconds), and
`max_output_bytes` (at most 65536). Changed executable bytes refuse dispatch. The program receives
JSON on stdin, no arguments, its installation directory as working directory, and an empty
environment plus sanitized PATH, HOME and XDG_CONFIG_HOME,
and supervised process-tree termination. It must own its credential access and enforce its
operation/destination scope. This is a trusted machine extension, not isolation of hostile code.

Requests (`af/scenario-adapter-request@3`) carry `method` (`reconcile`, `execute`, `observe`),
`intent_id`, `input_digest`, `operation`, `destination`, and typed `inputs`. Observations also
carry an injected text `condition` input. Responses (`af/scenario-adapter-response@1`) must echo
both identities and supply `outcome`, bounded `evidence`, and `observation` (null for actions).
An observation carries exact `subject`, `revision`, `condition`, `satisfied`, and `evidence`.

The installed adapter MUST implement durable idempotency by intent ID and exact input digest.
Before returning `done`, it must retain a service receipt that reconciliation can recover. A
`pending` outcome means the effect may exist and needs reconciliation. `absent` from reconciliation
means the service has authoritatively established that no effect exists; only then may af call
`execute` with the same key. `failed` means a definitive unsuccessful operation, safe for a new
bounded attempt. Timeouts, truncated output and lost connections never mean absent. Execute
must enforce exact object-revision preconditions, including checking the current head atomically
with publication where the service supports it, and refuse when it cannot establish them. This
contract, rather than process exit, prevents repeated PRs or follow-ups across restart.

An optional production [GitHub adapter](scenario-github-adapter.md) implements publication,
exact-head CI and merge observation, approved follow-up issues and bounded owner review requests.
Its separate offline installation pins complete application code and config identity. The local
simulator remains a credential-free test fixture; neither adapter is installed automatically.

The built-in actions use adapter `builtin` and an authorized existing canonical absolute file root:
`write_owned_file` takes a leaf `file` name (letters, digits, `-` and `_`, starting with a
letter or digit) and text `content`, writes a versioned ownership
wrapper atomically without replacing an existing file, and returns an `owned_file` receipt.
`cleanup_owned_file` takes that receipt as `owned`, checks device, inode, owner and complete
content digest, and removes only that file. Unowned and modified content is preserved; conflicting
recovery content remains in a named cleanup claim. Neither operation recursively removes a user's
workspace. Whole-workspace removal is outside the built-in cleanup scope. The optional
[Git workspace capability](scenario-git-workspace.md) supplies independently owned worktree/build
preparation and merge-gated cleanup through installed Actions.

The offline [`impl` recipe](scenario-impl.md) generates configurable consuming packages and
a finite implementation-to-human-merge Scenario using these ordinary capabilities.

## Manual bugfix workflow merge watcher

For a manually published bugfix/easy PR, the supported local
[merge watcher](scenario-merge-watch.md) can return control after background launch and remain
alive after green CI. It observes the exact human merge, verifies explicitly named local cleanup,
and writes the caller's durable `DONE` notification only after every cleanup target is absent.
It uses read-only GitHub inspection, retains Task Stores, and installs no scheduler. This separate
script does not change native Wait behavior or complete a native Scenario Run.

## Daily Ant scheduling

An `af/ant@4` definition has `id`, `scenario` (bundle), typed `inputs`, `owner`, `notification`,
`schedule` (`timezone`, `hour`, `minute`, `start_unix_ms`), and `execution`. The notification is
an accountable routing label retained in inspection; sending a notification is an explicit
`notify` action with its own authorized adapter.

Execution declares `overlap: skip`, `missed: skip`, `lateness_ms`, `per_run` limits,
`rolling_tokens`, and `rolling_window_ms`. The default policies are explicit and mandatory in
v1: one active Run, including waits; skip while busy; no catch-up burst. Rolling limits reserve
an active Run's whole token allowance regardless of age. Terminal consumption remains charged
through a rolling window after settlement, conservatively covering all dispatch within the Run.

Validate with `af ant validate --file ANT --authority POLICY --json`, review the exact pin, then
`af ant add --file ANT --authority POLICY --confirm-pin REVISION`. Add registers it disabled.
`af ant enable ID`, `pause ID`, `disable ID`, `show ID`, and `list` manage or inspect it. An
external scheduler calls `af ant tick`: due occurrences are recorded in one immediate database
transaction before newly admitted Runs are driven. Restart or concurrent ticks cannot launch
the same occurrence twice. Ticks also recover and drive existing active Ant Runs, polling bounded
waits; `scenario resume RUN` can drive one explicitly. A concurrent driver is reported as busy.
An individual Ant refusal is recorded under `refused`
in the tick receipt and leaves that Ant unchanged; other Ants can still progress.

Timezones are explicit `UTC`, a fixed offset such as `+03:00`, or a system IANA name such as
`Europe/Berlin`. IANA TZif transitions are captured in the Ant pin, independent of later tzdata
updates or ambient TZ. At a daylight gap the occurrence is recorded as skipped; at a repeated
wall time only its first occurrence is admitted. Where the TZif file supplies recurring future
DST rules, v1 uses its explicit transition horizon and refuses a tick beyond it. Review a new Ant
revision with current tzdata before that horizon. A tick processes at most 4096 calendar days of
downtime; longer downtime requires bounded intermediate ticks. Each occurrence has an identity
bound to Ant revision and local calendar date and a `started` or named `skipped` receipt.

Paused and disabled periods are retained as timed state transitions, so enabling later cannot
launch an occurrence that was paused when due. Launch also requires the Ant to be enabled at
tick time. Any pause or disable transition after due time and before the first tick prevents that
launch, even if the Ant is enabled again before the tick. The skipped occurrence remains skipped
on restart; later eligible days may launch normally. Historical due-time modes and missed
occurrences keep their original skipped outcomes.
Pausing does not cancel or release an active Run.
No real definitions, schedules, credentials or publication grants ship enabled in Afactory.

## Goal roadmaps: planning and complete-loop admission

`af scenario roadmap` retains a bounded optimization roadmap without running it. A partition is
one **product component**, **target metric**, or **optimization goal** that owns a complete loop:
research, a candidate if worthwhile, experiments, verification and reporting are subordinate
checkpoints. A pipeline stage or a calendar day is not a partition. A goal spanning several days
keeps the same stable ID; a revision or a later horizon never resets its accounting. Admission
explicitly defers a goal whose whole loop cannot fit. Only a reviewed proposal revision
may narrow product/workload scope; admission never silently changes the goal.

The proposal (`af/scenario-roadmap-proposal@2`) declares `id`, `objective`, `horizon` with Unix
millisecond start/end bounds (at most 366 days), positive roadmap-total `max_tokens`,
`daily_tokens`, `rolling_tokens`, `rolling_window_ms`,
and 1–256 `goals`. Daily accounting uses UTC; the Ant schedule retains its separate pinned timezone.
Every goal declares a stable `id`, `objective`, `partition` (`product_component`, `target_metric`
or `optimization_goal`), `component`, product `scope`, bounded `workload`, and `metric` with
`name`, `direction` (`decrease`, `increase`, `maintain`) and `unit`. Nonempty `acceptance`,
`guardrails` and `remaining_work` describe the whole loop. `priority` is a positive integer;
`dependencies` and `conflicts` name other stable goal IDs. Unknown/self/duplicate links,
dependency cycles and a direct dependency that is also a conflict are refused.

Each goal declares positive `loop_tokens`: the authorized allowance for its complete optimization
loop, including every declared Task attempt. `estimated_tokens` is a separate untrusted planning
estimate, null when unknown, bounded by `max_tokens`.
It is never spending evidence or a reservation. `proposed_eligible` is a proposal, not admission;
false requires a `deferral_reason`. `sources` are untrusted leads, each with a SHA-256 `digest`
and `kind` (`source`, `research`, `candidate`, `experiment`, `verification`, `report`). A reference
labelled verification or experiment does not establish acceptance or a completed experiment.
These references never cause af to fetch or execute anything. Token bounds are safe JSON
integers; malformed references, blank criteria and invalid horizons fail validation.

The inert example is executable from the repository root. Review the full validation output
before using its exact revision as confirmation:

```sh
cp fixtures/scenarios/performance-roadmap.json roadmap.json
af scenario roadmap validate --file roadmap.json --json
revision=$(af scenario roadmap validate --file roadmap.json --json |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["revision"])')
af scenario roadmap create --file roadmap.json --confirm-revision "$revision" --json
af scenario roadmap show performance --json
af scenario roadmap list --json
```

Edit future planning in `roadmap.json`, then review and confirm its successor revision:

```sh
python3 - <<'PY'
import json
with open("roadmap.json") as f:
    proposal = json.load(f)
proposal["goals"][0]["estimated_tokens"] = 3000
with open("roadmap.json", "w") as f:
    json.dump(proposal, f)
PY
af scenario roadmap validate --roadmap performance --file roadmap.json --json
revision=$(af scenario roadmap validate --roadmap performance --file roadmap.json --json |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["revision"])')
af scenario roadmap revise performance --file roadmap.json --confirm-revision "$revision" --json
```

Validation alone creates no Store. Creation and revision are token-free, deterministic local
journal operations. They launch no model, Task, benchmark, adapter, Scenario Run or Ant; they
install no schedule and grant no publication authority. They accept TOML or JSON by filename,
using the same strict format selection as Scenario definitions. `--state` selects the existing
private Scenario Store outside checkouts. `--json` emits `af/scenario-roadmap-revision@2` for
validate, `af/scenario-roadmap@2` for create/show/revise, or `af/scenario-roadmap-list@2` for list;
text validation prints the revision. `af schema show ID` provides the shipped contracts.

Creation allocates an opaque roadmap identity separate from its local label; a goal's stable
identity is the pair of roadmap identity and goal ID, including across Stores with equal labels.
New goals always start pending with zero reserved/actual tokens and no execution evidence.
Progress is runtime-owned: `state`, `reason`, `claim`, `captured_goal`, `reserved_tokens`,
`actual_tokens`, `task_charges`, `task_receipts`, checkpoint `artifacts`, and `completed_work`
cannot
be supplied in proposals.
There is no manual completion or receipt-import verb. Native accepted Task receipts remain the
acceptance authority; a roadmap does not introduce another Task acceptance engine. Native Task
charge records keep unresolved work reserved and retain failed work separately from
accepted Task receipts; totals are derived from those records and a Task charge appears in only
one goal. Actual tokens are native recorded charges, not inferred provider spending; absent
estimates stay unknown.

Each revision retains its exact proposal, parent and content identity. Revision confirmation
binds the proposed content and current parent; a concurrent writer makes a stale confirmation
fail, requiring validation again. Every previous revision and runtime receipt remains retained.
Goal IDs cannot be removed or reused: defer future work instead. Pending goals may be respecified;
active/completed goals retain their captured definition. Revisions cannot reset reservations,
charges, evidence or completed work, or set a future token bound below committed accounting.
Recorded overruns remain readable evidence; they never disappear because they exceed a bound.
Creation refuses an existing label. Replay checks the revision chain and accounting; old shapes,
missing ownership and corrupt journals fail closed.

Roadmaps share `runs/scenarios.sqlite`, its immediate transactions, private ownership and checksum
with Runs and Ants. The journal is now `af/scenario-store@5`; the previous Store shape is refused
without migration. The unchanged 16 MiB document bound includes all roadmap history. The ordinary
machine Storage Budget counts these bytes as retained Store remainder; pruning never silently
collects revisions or resets a ledger. No second registry or storage budget exists.

Fresh Task instantiation and complete-loop admission are native. The bounded asynchronous
Performance Lab adapter uses an explicit `experiment_submit_v1` Action, exact accepted Snapshot
handoff and native verified completion. Research receives immutable bounded evidence packets;
typed `scenario_evidence` observations enter fresh Task Requirements. See the executable inert
bundles and [manual-first deployment runbook](performance-scenarios.md). The operator must perform
one complete manual loop on Claw before separately authorizing its pinned Ant schedule there.
Installation, real benchmarks and VM changes remain operator work; no Mac scheduler is supplied.

## Fresh Task templates

`af scenario template-pin` captures an `af/scenario-task-template@1` and its digest in an
`af/scenario-template-pin@1` envelope. It is token-free and writes only native Task Store capture
artifacts; it creates no Task or Scenario Run. Supply a native Task file with explicit pipeline,
strategy and all limits, an exact 40-character authority commit and an external Task Store.
Implementation files also declare verification; other kinds retain their captured native acceptance.
Pipeline fallback must be `refuse`; generated/default plan selection is unauthorized.
An optional native bindings file may pin slots and Providers, but may not supply local package
paths. Source files, catalog packages, pipelines, policies and Worker settings come from the exact
committed authority; the template retains their native authority identity, repository identity
and source content digest. A base, if supplied, must also be an exact commit. Templates cannot
prefill native inputs, continuations or issue authority.

```text
# needs: a reviewed native Task file, committed authority and external Task Store
 af scenario template-pin --file task.json --repo /project --task-state /private/tasks \
   --authority-commit 0123456789abcdef0123456789abcdef01234567 --json
```

Copy the returned `template` object into a step with `operation.kind: fresh_task`. Its `bindings`
map is empty or maps each native root input name to `port` (a predecessor output name) and
`artifact_type` (its exact native type). The corresponding Scenario step input must be a Task
receipt from a preceding step or child port. For example, a `source` binding names output
`snapshot` with type `af/SourceTree@1`. Both `authority.task_templates` and machine policy
`task_templates` must contain the template digest. Children inherit a subset of template grants.
The inert `fixtures/scenarios/fresh-task-loop.json` demonstrates the wire form; replace its
placeholder pins through the capture command before use.

Dispatch reserves the Task allowance and commits an intent before native planning. The fresh Task
ID is derived from the opaque Run/step/attempt intent, not from a user label or date. Native
planning captures predecessor references through the ordinary Task input resolver. Only the exact
completed, accepted result in that same Task Store can bind; missing outputs, wrong artifact
types, foreign repositories and invalid Snapshot provenance are refused. A reviewed candidate
from an unaccepted Task cannot enter this path as accepted evidence. Native re-rooting retains
its original provenance, including the predecessor's acceptance and conclusion.

Before confirming and running the resulting exact native plan, the coordinator records its Task,
revision and plan IDs. A lost planning response reconciles the same Task file and stable Task ID;
resume never invents another Task. It also checks the captured template authority again, including
catalog, pipeline, Provider bindings and source identities. Changed or missing native authority
fails closed. Editing an authoring file cannot change an already pinned template. A definitive
failure may use a fresh Task under the step's finite next attempt, with every earlier charge kept.
Existing exact-plan `task` steps remain supported in the current Scenario shape.

## Claiming complete roadmap goals

Set the root Scenario's `admission` to an object with `roadmap` (local label), `identity` (opaque
roadmap identity) and `revision` (the exact reviewed roadmap head). Use null for an ordinary Run.
A child Scenario cannot claim another roadmap. A roadmap loop uses fresh Task steps throughout
its pinned bundle;
existing Task receipts cannot be recycled to complete another goal. Every fresh Task receives the
complete captured goal as structured requirements under `scenario_goal`.

Manual `scenario start` and scheduled Ant admission use the same immediate transaction. It sorts
eligible goals by positive priority then stable ID, claims one, and reserves its whole
`loop_tokens` before dispatch. Dependencies require completed native evidence. Conflicts are
symmetric; an active, uncertain or completed goal also blocks a duplicate product partition,
even under a new goal ID or revision. Each deferral retains an inspectable
reason. The allowance must cover all declared Task attempts and fit per-run, UTC daily,
roadmap-total and rolling bounds simultaneously. An impossible goal invokes no model. A manual
Run records failure when nothing fits; an Ant records `skipped_roadmap` with its reason. Repeated
starts/ticks cannot duplicate a claim or occurrence.

An active or uncertain claim holds its complete unused allowance regardless of age. Terminal
known charges count on both admission and settlement UTC dates, and through a rolling window
after settlement. Known tokens from failed Tasks remain actual charges. Native unknown-usage
Attempts retain uncertainty even when the Task is finished: known tokens and the unused loop
reservation are kept separately. A deadline, cancellation, midnight, new occurrence, schedule
replacement or roadmap revision cannot erase them. Native result reconciliation can settle a
response lost before the Scenario recorded it; no CLI accepts a supplied charge or completion.
Run resume retains its original claim and revision even after future planning changes.

Only successful pinned Scenario completion with accepted native Task receipts and settled
accounting completes a goal. A failed or uncertain loop remains deferred with its claim retained;
it is never automatically re-claimed under another date or revision. A future revision can change
unclaimed planning, but cannot replace captured goals or accumulated accounting. Roadmap history,
claims and native receipts share the private Store and its bounded journal and Storage Budget.

Performance roadmap implementation and verification model plans must use Sol (`gpt-6.1-sol`) medium
for implementation, Terra
(`gpt-6.1-terra`) high for bugs, Sol (`gpt-6-sol`) high for correctness, and Astra
(`gpt-6-astra`) medium for evaluation, all on `codex-personal`. Verification is read-only and has
no implementer. An advisory model role or a default-light judging team is refused before model
dispatch. Command-only native fixtures remain token-free and use no Provider.

The [native performance loop decision](adr/2026-11-03-native-performance-scenario-loops.md)
extends ADR-0274 with complete-goal admission, fresh Tasks and the asynchronous lab adapter.
`af scenario evidence-source TASK_ID --artifact-id ID --repo REPO --task-state STORE --json`
retrieves one native findings, History Capture or adoption artifact after checking its recorded
Task/source receipts. It reads no owner-supplied JSON artifact file and launches no Worker.
