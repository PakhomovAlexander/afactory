# Paired source navigation benchmark

The installed CLI runs credential-free paired navigation smoke on an immutable SourceTree.
Both arms use the same source and case. The synthetic subject takes scripted steps.
The native fixture subject sends the prompt through the pinned Codex app-server.
Both modes keep the synthetic anchor oracle outside the source given to the subject.

## Synthetic smoke

Preinstall the [broker's pinned image](design/agent-bench-broker.md#docker-lifecycle-and-containment)
and an executable Linux rust-analyzer from Rust 1.88.0 that runs in that image. Supply the SHA-256
of the native executable. A rustup launcher will fail backend execution.

```sh
af agent-bench plan --smoke --rust-analyzer "$ANALYZER" \
  --rust-analyzer-sha256 "sha256:$ANALYZER_SHA256" \
  --rust-analyzer-library-dir "$RUST_LIB" --json
af task run "$TASK_ID" --confirm-plan "$PLAN_ID"
af agent-bench report "$TASK_ID" --json
```

Set `TASK_ID` and `PLAN_ID` from the plan output. Supply the same `--repo` and `--state` to each
command if selecting a Store explicitly. Smoke generates a fresh two-crate source fixture
and three adjacent arm pairs covering definition, references and implementation. The Task
records all six allocations before it admits any Attempt.

The library directory supplies the Rust driver and LLVM that the pinned Python broker image
lacks. AF admits the exact Linux analyzer and both libraries by SHA-256 and mounts only those
runtime files read-only for a treatment semantic call.

## Native Codex fixture smoke

Add `--native-fixture-codex "$CODEX" --fixture-endpoint 127.0.0.1:PORT` to the smoke plan.
Supply the native Linux x86-64 Codex 0.167.2 executable and the same pinned analyzer library
directory. Run and replay the plan using the commands above. The endpoint must be a local
credential-free Responses fixture. Codex uses an inert auth file in a fresh private home.

Each turn receives the common anchor-answer instruction and one original smoke question.
Codex selects the tools and positions. Both arms register list, read and search. Treatment
also registers definition, references and implementation. The gateway checks the actual
registration and refuses tool calls outside that arm's catalog.

The native fixture reserves a bounded Attempt per slot and acknowledges each intent and
raw Responses record after its bytes are durable in the Task CAS. Attempt settlement links
the retained objects. A lost worker before settlement can leave durable objects without a
settlement link. Replay leaves those observations unknown. Reported native fixture tokens
sum input and output. AF charges zero because the endpoint is a dummy, and the report keeps
that charge distinct from fixture usage.

The six-slot fixture uses the synthetic anchor grader.

## Frozen prompt-only experiment

Use the Git checkout containing the frozen source revision and keep the subject package,
schedule and Rust crate graph outside the source commit.

```sh
af agent-bench plan --repo "$SOURCE_REPO" --source-revision 02460368505f33c60f37e5333a15ac3541ad14f0 \
  --bundle "$SUBJECT_JSON" --schedule "$SCHEDULE_JSON" --phase smoke \
  --project-graph "$GRAPH_JSON" --project-graph-sha256 "$GRAPH_SHA256" --json
af agent-bench report "$TASK_ID" --repo "$SOURCE_REPO" --json
```

The subject package contains `schema`, `common_instruction` and twelve `cases` with only
`id` and `prompt`. The schedule contains `schema`, `status`, `seed`, `ordering` and ordered
`blocks`. Each block specifies `pair_id`, `case_id`, `repetition` and its exact two-element
`order` of V1 and V2. Each case N01 through N12 occurs once per repetition across three
repetitions. Six blocks per repetition start with V1. The planner preserves listed order and
keeps each pair adjacent. The crate graph is a separate JSON array of the same explicit Rust
crate records used by the synthetic benchmark. It supplies crate roots, editions and dependency
edges for the broker.

The frozen commit requires the built-in SHA-256 pins for both the subject and the schedule.
For other source commits, supply explicit `--bundle-sha256` and `--schedule-sha256` flags.
The Task Store retains the immutable SourceTree and all three original JSON files with
both their raw SHA-256 pins and their Store object IDs.
Replay requires the same Store selectors and reads the captured bytes independently of the
original input paths. The [external plan schema](../schemas/agent-bench-external-v1.json)
defines the new plan and report formats.

Smoke selects the first two pairs. A/A and A/B capture all 72 slots. A/A uses the baseline
tool set for both labels. A/B and smoke use baseline tools for V1 and add the three semantic
operations for V2. The offline report preserves the common instruction, each complete question, and the exact tool
set identity for every unstarted slot. Timing, usage and grades are absent for unstarted slots.

For an executable A/A plan, pass the completed smoke **grading** Task ID with
`--predecessor TASK_ID`. Use the A/A grading Task ID for A/B. Use the same Store and exact input
pins throughout. AF recomputes readiness from the subject and grading ledgers. Omit the
predecessor and transport flags to inspect a plan with a pending prerequisite.
Use `af task run` with the exact execution plan ID to confirm each phase.

### Admit an external subject run

Add a pinned transport declaration and the analyzer executable and library flags from synthetic
smoke to the external plan command. The strict JSON declaration has the following fields.

```json
{
  "executable": "/absolute/path/to/codex",
  "executable_sha256": "sha256:<native executable digest>",
  "backend": {
    "kind": "codex_personal",
    "account_binding": "sha256:<native ChatGPT account ID digest>",
    "auth_homes": ["/absolute/path/auth-1", "/absolute/path/auth-2", "/absolute/path/auth-3", "/absolute/path/auth-4"],
    "catalog": "/absolute/path/to/restricted-model-catalog.json",
    "catalog_sha256": "sha256:<externally captured metadata digest>"
  }
}
```

Use `--external-transport "$TRANSPORT" --external-transport-sha256 "$TRANSPORT_PIN"` with
a `sha256:`-prefixed digest of the declaration's exact bytes. The live mode uses
Codex Personal at the fixed official upstream with the pinned 0.167.2 native app-server and
`gpt-6.1-sol` at medium effort. The operator supplies the SHA-256 of the native ChatGPT account ID bytes.
AF checks that binding before forwarding any official request.
Each home must be a private, canonical directory outside the checkout and Store with only a
private regular `auth.json`. Native Codex reads that file. AF stores the declared paths
and pins. Use one auth home per slot because Codex can write runtime files in its home.

```sh
af task run "$TASK_ID" --confirm-plan "$PLAN_ID" --repo "$SOURCE_REPO"
af agent-bench report "$TASK_ID" --repo "$SOURCE_REPO" --json
```

Use the execution plan ID from the plan command and keep the same Store for the frozen live
pilot. The Store gives each live phase and its grading Task a single identity. Smoke admits four
ordered subject slots. Each A/A or A/B phase admits 72 with one fresh auth home per slot.
Each subject stops before the next send at 100,000 inclusive input and output tokens.
An overshooting, failed or unknown response stops collection. The smoke reservation is at most
ten minutes per subject, including exit and drain. Full phases reserve six minutes per subject
so all 72 reservations fit within the eight-hour run limit. An interrupted run retains the
send intent, any drained response, and the
ordinary Attempt's charge and status. If a partial live bill invokes AF's reservation floor,
the report exposes the known Provider charge separately. The report retains the original bounded answer text,
including any abstention and source citations supplied by the subject, for later grading.

The credential-free declaration uses backend
`{"kind":"loopback_fixture","endpoint":"127.0.0.1:PORT"}` and the pinned Linux Codex executable.
It uses the same external phase, broker, Attempt and replay path with inert native auth and
zero AF charges. Fixture subjects also have individual 100,000-token limits. Only a grading
Task from the same backend can supply a predecessor. The offline report identifies fixture
mode and preserves native usage separately.
The [external phase schema](../schemas/agent-bench-external-v3.json) specifies the executable
plan summary, binding declaration and report.

## Synthetic external cases

Replace `--smoke` with `--source-revision COMMIT --cases CASES --cases-sha256 DIGEST
--oracle ORACLE --oracle-sha256 DIGEST`. `COMMIT` is a full Git commit ID in `--repo`. Both JSON
inputs pin the same native Manifest content digest. The case file declares a bounded explicit
Rust crate graph, case questions, UTF-16 LSP positions, literal search terms, operations and
repetitions. The separate oracle maps each case ID to its exact ordered list of one-based
path and line anchors. The [native schema](../schemas/agent-bench-native-v2.json) describes
their fields and the report. Only the explicit source commit is staged. Keep the oracle, its
derivatives, answer keys and benchmark instructions outside that commit.

Plans bind the source, analyzer bytes, broker configuration, case package, oracle and stable
pair order in the Task Store. The synthetic subject receives one case and its arm's tools.
Treatment stages a fresh analyzer and uses an explicit crate graph inside the source-only
Docker broker. The read-only container has no network or credentials. Cargo autoload,
build scripts, proc macros and check-on-save are disabled. Source-local rust-analyzer
configuration is refused. The broker refuses result paths outside the Manifest, caps replies,
and marks excess anchors as truncated. Explicit local Rust crate graphs are supported.
Build-time generated code, proc macros, sysroot indexing and Cargo dependency resolution are
outside this slice.

## Evidence

The ordinary Task runtime reserves and settles each Attempt. Report replays the checked log
and CAS offline, including failed, missing and unstarted slots. A failed subject leaves the
Task unverified and its observations remain available in the report. Each retained elapsed time
includes broker staging, container setup, cold indexing for treatment, tool replies and
cleanup. A pair delta requires elapsed observations from both slots with no earlier started
Attempts for either slot. AF charges come from the Task ledger. Synthetic inference leaves
inclusive native token measurements null, including with synthetic external cases. The native
fixture report labels its dummy endpoint and withholds inclusive totals for slots with missing
usage or multiple started Attempts. Timing and usage are descriptive fixture observations.

The credential-free integration test with real containment requires Docker and the same
native analyzer:

```sh
AF_TEST_RUST_ANALYZER_1_88="$ANALYZER" AF_TEST_RUST_ANALYZER_LIB_DIR="$RUST_LIB" \
  cargo nextest run --workspace --locked --profile ci \
  --run-ignored all -E 'binary_id(=af::it) & test(=agent_bench::installed_cli_runs_confined_rust_analyzer_for_treatment_only)'
```

With the same environment and `AF_NAVIGATION_CODEX="$CODEX"`, use the filter
`binary_id(=af::it) & test(=agent_bench::installed_native_fixture_cli_replays_both_arms_and_dummy_usage)`
to verify the native smoke plan, run, and replay against a local dummy endpoint.

With the same environment, run
`binary_id(=af::it) & test(=agent_bench::installed_external_smoke_uses_two_turns_and_settles_offline)`
with `--run-ignored only` to check the external phase through the installed CLI. It requires
IPv4 loopback and Docker. The installed external planning test
`agent_bench::installed_external_smoke_plan_replays_without_original_bindings` also checks
offline replay after a credential-free transport failure.

## Check a private evaluator package

```sh
af agent-bench check-evaluator "$TASK_ID" --repo "$SOURCE_REPO" \
  --evaluator "$EVALUATOR_JSON" --evaluator-sha256 "$EVALUATOR_SHA256"
```

Use the same Store selectors as the captured external Task. Keep the evaluator file outside the
checkout and Store. The command checks the raw SHA-256, exact SourceTree Snapshot, and every
case in the captured prompt package, including cases outside the four smoke slots. Frozen source
requires the pinned production bytes. For a synthetic source use the
[synthetic evaluator schema](../schemas/agent-bench-evaluator-v1.json). The backing report
SHA-256 in the frozen package is bare lowercase hex. Its report and
verification artifact references are typed SHA-256 identities. The references are declarations
in the pinned package. The check reads only the package and retained Task authority.

The command prints a validation result without including oracle or rubric text. It leaves the
evaluator file with the operator and creates no Task artifacts, grants, grades or phase evidence.

## Grade retained answers

With a finished external fixture Task in the same Store, capture a separate grading plan.
Supply a synthetic evaluator for its exact source and a credential-free IPv4 loopback Responses
fixture. Use the pinned native Linux Codex executable.

```sh
af agent-bench plan-grading "$SUBJECT_TASK_ID" --repo "$SOURCE_REPO" \
  --evaluator "$EVALUATOR_JSON" --evaluator-sha256 "$EVALUATOR_SHA256" \
  --native-fixture-codex "$CODEX" --fixture-endpoint "127.0.0.1:$PORT"
af task run "$GRADING_TASK_ID" --confirm-plan "$GRADING_PLAN_ID" --repo "$SOURCE_REPO"
af agent-bench report "$GRADING_TASK_ID" --repo "$SOURCE_REPO" --json
```

Each grader runs a new native Attempt at `gpt-6.1-terra` high. Grader sessions receive the
original question, retained answer with its citations, common rubric and its one case oracle.
They share access to the captured source through the text navigation broker. The adjudicator
receives the same projection in a fresh session if both independent judgments disagree. The
host retains the opaque association and records that verbatim answer text can reveal treatment.
The final stage seals the decision from the judgments. Offline replay joins it to the original
subject slot and includes the sends, settlements, unknowns and unavailable stages.

If a grader exceeds its admitted inclusive allowance or returns unknown usage, the batch stops
admitting native requests across all remaining slots and stages. The report retains the failed grade,
its raw response and any known usage. Unavailable stages retain their place in the denominator,
and an incomplete batch cannot qualify a successor. Replanning the same grading inputs returns
the captured plan. Running a finished Task again returns its recorded result.

Use the same Store selectors throughout. The plan retains each blind projection for offline
replay after the original evaluator is removed. Grade and subject native usage appear separately.
Fixture settlements record zero Provider charges. A complete fixture grade can qualify only a
fixture successor.

In ordinary Task status, verified collection means every subject answer was retained. Verified
grading means every slot has an independently resolved decision. Both reports identify the
transport mode. Phase readiness additionally checks the pilot overhead and the applicable paired
screens.

For a confirmed live subject Task, use `--grading-transport FILE --grading-transport-sha256 PIN`
in place of the fixture flags. Supply the pinned production evaluator only for an operator run.
The live grading declaration has `executable`, `account_binding`, `auth_homes`, `catalog` and
`catalog_sha256` fields. The executable must match the pinned Linux binary. Use a restricted
Terra catalog and one fresh, private auth home per potential stage, including the adjudication
stage. This is 12 homes for smoke and 216 for a full phase. The official gateway checks the
account digest against the header native Codex actually sends. The Store retains catalog bytes
and declaration fields. It does not copy the auth files into CAS.

Reports preserve the exact phase and slot count, settled decisions, native usage and elapsed
observations. The pilot pools grading and other recorded overhead separately from subject
tokens and time, with limits of 4,000,000 inclusive tokens and 440 minutes. A/A checks complete
collection and resolved grades for every scheduled slot and screens both resource measures
using all 36 paired blocks. It weights repetitions within case, cases within family, and
families equally. The exact absolute relative differences must be at most 20 percent.
A passing A/A screen establishes readiness for the next phase in the same mode.

The installed conformance test
`agent_bench::installed_external_grading_blinds_native_attempts_and_replays`
requires the pinned Linux Codex executable, analyzer runtime and IPv4 loopback. It uses fixture
answers and bills. The absent-answer conformance test exercises the offline report after a
refused transport.

The offline analyzer accepts a tool-only A/B schedule with one shared immutable source, model,
configuration, collector and protocol. The two catalog references identify the baseline text
tools and the treatment set with Rust semantic navigation added. The analyzer uses the same
paired weighting and missing-data rules as the map experiment.
