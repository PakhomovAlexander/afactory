# Scenarios and Ants

A Scenario orchestrates existing Tasks, child Scenarios, bounded actions, signed human decisions,
and observations. An Ant binds a pinned Scenario to a daily schedule. Neither command installs a
cron job, starts a daemon, or creates an instance until explicitly requested. Models run only
inside Tasks. [ADR-0274](adr/0274-orchestrate-pinned-scenarios-and-admit-ant-occurrences.md)
defines the authority boundary.

## Review, record, drive

Definitions use TOML for authoring and JSON for their versioned wire documents, with schema
`af/scenario@1`. A bundle (`af/scenario-bundle@1`) names its
`root` and maps names to definitions. See the inert examples in `fixtures/scenarios/` and the
schemas from `af schema show`. Definitions have explicit `inputs`, `outputs`, ordered `steps`,
`result` bindings, boolean `completion` conditions, `authority`, and `limits`.

The port types are `text`, `boolean`, `integer`, `task`, `action`, `decision`, `observation`, and
`owned_file`. A value is `{"type":"text","value":"example"}`. Bindings have `source` set to
`literal` (with `value`), `input` (with `port`), or `step` (with `step` and `port`). Forward
references and type mismatches fail admission. A `project` binding selects a typed receipt field:
`task_conclusion`, `task_result_id`, `task_output_id` or `task_output_snapshot` (the latter two
name a `port`), `observation_satisfied`, or `decision_approved`. Its `value` is another binding
and its `projection` is an object with `kind`. An `equals` binding compares scalar `left` and
`right` bindings of the same type and returns a boolean. This allows a Task's evidenced
`no_worthwhile_optimization` conclusion to skip implementation and still complete successfully.
Root inputs and literals are scalar data; they
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

`af/scenario-policy@1` is an explicit, owner-only machine file outside every Git checkout.
Author it as TOML; its captured JSON wire form is also accepted. The filename selects the format
(`.toml` or JSON); a parse failure never selects a fallback format or an earlier version.
It contains
`task_plans`, `decision_keys`, `file_roots`, and `adapters`. The pin includes the entire effective
policy. No workflow supplies executable paths, arguments, environment variables, or credentials.
Each adapter is an installed, trusted program outside the checkout, pinned by executable SHA-256,
with finite `operations`, exact `destinations`, `max_wall_ms` (at most 30 seconds), and
`max_output_bytes` (at most 65536). Changed executable bytes refuse dispatch. The program receives
JSON on stdin, no arguments, its installation directory as working directory, and an empty
environment plus sanitized PATH, HOME and XDG_CONFIG_HOME,
and supervised process-tree termination. It must own its credential access and enforce its
operation/destination scope. This is a trusted machine extension, not isolation of hostile code.

Requests (`af/scenario-adapter-request@1`) carry `method` (`reconcile`, `execute`, `observe`),
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
workspace. Whole-workspace removal is outside the built-in cleanup scope.

## Daily Ant scheduling

An `af/ant@1` definition has `id`, `scenario` (bundle), typed `inputs`, `owner`, `notification`,
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

A [structured Task example](../fixtures/custom-tasks/README.md) projects the exact typed output
of one captured issue Task through `task_output_id` and checks `structurally_validated`. This
uses the same pinned Task projection as every other kind, without report or evaluator nodes,
dynamic Task creation or publication authority.
