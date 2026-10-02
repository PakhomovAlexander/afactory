# Task input bindings

A Task file can bind a root input port to an output of a Task already recorded in the same Store,
instead of exporting that output to a file and capturing it again. The decision is
[ADR-0117](../adr/0117-bind-task-inputs-to-recorded-task-outputs.md), widened by
[ADR-0134](../adr/0134-bind-any-declared-root-port-to-recorded-task-outputs.md) to every root
input the selected Pipeline declares; this page is their Task-file reference and a map of where
they are implemented.

## The shape

One optional top-level table, `inputs`, maps a root input port name to a reference:

```json
{
  "schema": "af.task-file/1",
  "task_id": "layout-l3b-repair",
  "kind": "implement",
  "goal": "Fix the Findings the reviewers raised on layout-l3b.",
  "inputs": {
    "source":  { "task": "layout-l3b", "port": "snapshot" },
    "history": { "task": "layout-l3b", "port": "history" }
  },
  "strategy": "standard",
  "verification": "review",
  "facts": { "standard": true },
  "limits": {
    "tokens": 900000,
    "max_attempts": 4,
    "wall_ms": 7200000,
    "verification": { "tokens": 300000, "attempts": 2, "wall_ms": 1800000 }
  }
}
```

| Form | Meaning |
|---|---|
| `{ "task": "<task_id>", "port": "<output port>" }` | The named output port of a Task recorded in the Store selected by `--state`. Both members required. |
| `{ "artifact": "sha256:…" }` | One artifact in the same Store, by exact ID; cardinality `one`. Binds only `source`, `history` and `sources`. |
| `[ { "task": …, "port": … }, … ]` | One to sixteen recorded outputs gathered, in order, into one `many` port. |

A report Task reading an experiment binds all three kinds of port:

```json
"inputs": {
  "comparison":   { "task": "experiment", "port": "comparison" },
  "measurements": [ { "task": "experiment", "port": "baseline" },
                    { "task": "experiment", "port": "candidate" } ],
  "source":       { "task": "experiment", "port": "snapshot" }
}
```

Any root input the selected Pipeline declares is bindable, except `requirements`, `base` and
`continuation`, which are refused by name. At resolution the selected Pipeline is the one the
Task file names in `pipeline`; when it names none, every captured Pipeline accepting the Task's
kind is consulted and must declare the port alike, or the refusal asks for the Pipeline to be
named. A port none of them declares is refused by name at plan time.

Three ports are the ones the Task-file adapter constructs — `source`, `history` and `sources` —
and a binding replaces that construction: a bound `source` captures no Git tree from the
invoking checkout (so `--uncommitted`, which captures exactly that, is refused together with a
bound `source`), a bound `history` suppresses the `empty_review_history` root default, and a
bound `sources` makes `document_sources` — or, for a report Task, whose `sources` is an
`af/ReportSources@1`, `report_sources` — unnecessary. Their expected type is this profile's, as
ADR-0117 fixed it; every other port's is the selected Pipeline's declaration.

The referenced Task must be recorded in this Store, finished, its result must carry the named
port, and every artifact the port names must verify in the CAS. Only result outputs bind: a name
the result does not carry — `raw_artifact_ids`, `runtime_evidence` or any other record — is
refused with a message saying so, and an exact artifact ID, which could name either, binds no
port beyond ADR-0117's three. An unverified Task's `snapshot` may be referenced; the reference
carries provenance only.

Type and cardinality are checked against the port twice — once by the adapter against the
profile's type or the declaration above, once by the compiler against the Pipeline actually
selected — both before any Worker or Provider admission. For a single reference, cardinality is
exact equality of the recorded and the destination cardinality: a `many` output never binds a
`one` port, however many artifacts it currently holds, and a `one` output binds a `many` port only
as a list. A list binds only a `many` port; each listed output must carry the port's type, and
their artifacts, in order, must be distinct. A refusal is an ordinary Task-file input error that
names the port, the reference and both types where they differ: exit 1 with the `af/error@1`
document under `--json`, and no Task is recorded.

## What resolution records

Resolution happens at plan time, once, and only from the `--state` Store. The compiled plan holds
exact artifact IDs, so `af task run`, resume, retry and replay never read the referencing Task
file again, and an edited `inputs` table makes a new revision that invalidates any plan approval.

`history` and `sources` carry the referenced artifact ID verbatim and name no Snapshot. Every
other declared port carries the referenced artifacts verbatim too: a `one` port — or a `many`
port bound from one output — keeps that output's Snapshot ID, and a `many` port bound from
several outputs names **no** Snapshot, because a port names one and an experiment's `baseline`
and `candidate` measured two. Each artifact keeps its own subject Snapshot in its envelope, and
a Worker sees each value with its own. The Store accepts such a port only as an input — of a
revision, a plan, an invocation, or the root inputs receipt that equals them — and the executor
only where the consuming contract declares it `unbound`; every output port still names the one
Snapshot its artifacts are about.

A bound `source` is admitted
before anything is decided about it: the referenced `af/SourceTree@1` envelope's payload, its
`subject_snapshot_id` and the recorded port must name one Snapshot, whose origin must read and —
for a root capture, which is the one delivery compares — describe that tree. An envelope naming
one Snapshot in its subject and another in its payload is refused rather than resolved to either.
Only an admitted source reaches the branch below, which depends on one property of its Snapshot:

- **Already a root capture** — carried verbatim, keeping its `source_revision`. The Task delivers
  exactly as one planned from the checkout.
- **Derived**, which is what a `snapshot` output is — re-rooted: the referenced Manifest is
  republished as a root `af.task-snapshot/1` with no parent and an `af.task-source-origin/2`
  origin that copies the repository identity forward, states the bound tree's content digest,
  names the referenced Task, result, port and artifact under `bound_from`, and deliberately
  carries no `source_revision`. A new `af/SourceTree@1` envelope is published for the re-rooted
  Snapshot, and the resolved port carries that new artifact ID with the re-rooted Snapshot ID.
- **Already re-rooted** — a parentless Snapshot with a generation-2 origin, referenced again — is
  carried verbatim; nothing is re-rooted twice, and delivery refuses as for the first re-rooting.

A derived tree has no commit, so `af task deliver` refuses that second case, naming the Task the
*current* revision's binding referenced — or the artifact, when that binding named one directly:
there is nothing for the target repository's `HEAD` to equal, and
[ADR-0031](../adr/0031-deliver-verified-tasks-to-new-local-worktrees.md) compares the target
against the Task's own source Snapshot exactly. Commit the tree and plan the successor from the
commit if the result must be delivered. Bindings on `history` and `sources` do not affect
delivery.

### What a bound `history` can feed

The binding hands the successor's Pipeline the exact `af/ReviewHistory@1` the predecessor's
Review produced. What that Pipeline may do with it is bounded by a rule that predates this
decision and is not relaxed here: a Review Round is *restored* by recomputing it, and the
recomputation requires every reviewer result it reads to retain the **current** Task's
`af/Requirements@1` (`crates/review-pipeline/src/task/review.rs`, "Review result lost the exact
Task Requirements"). A predecessor's reviewer results cannot, because the requirements artifact
is derived from the Task file. So a `Recorded` history bound into a Pipeline whose Review would
continue that Round is refused by that check, on a Task whose root declares `requirements` —
which every `implement` Task does.

A bound `history` is therefore for a Pipeline that **reads** the ledger — a repairer or
implementer given the findings as context, which is what `docs/design/af-layout-plan.md` §3
describes — rather than one that continues the Round over a new Subject. Continuing a Round
across a Task boundary needs its own decision about requirements retention; this one does not
make it. `fixtures/task-runtime/bound-inputs/` is built that way, and says so.

## Where a binding is shown

- `af task explain` annotates the `IN` line (`source <- task layout-l3b/snapshot`, or
  `source <- artifact sha256:…` for an exact-artifact binding, or `measurements <- task
  experiment/baseline + task experiment/candidate` for a list); `--tree` adds one `BOUND` row per
  bound output with the referenced Task, port, acceptance, domain conclusion and exact artifact
  ID, or `artifact` and `-` where no Task was named. Every one of those goes through the preview's
  sanitizer, which maps any non-ASCII character to `?`, so the "nothing to show" marker is an
  ASCII hyphen.
- `af task show` prints one `bound <port> <- task <id>/<port> (<acceptance>)` line per bound
  Task output — two for a port bound from two — and one `bound <port> <- artifact sha256:…` line
  per exact-artifact binding, and its
  `--json` document carries `input_bindings` — present only when there is a binding, inside the
  one `af/task-inspection@11` generation. The self-optimizer's AF history adapter reads it as
  provenance that changes no counter.
- The durable record is one `af/TaskInputBindings@1` artifact referenced from the revision's
  `provenance.input_artifact_ids`, one entry per bound port. A port bound from several outputs
  keeps the first in its entry and every further one, in order, in an `also` list of
  `{ artifact_id, snapshot_id?, task }`; `also` is absent for every other binding.
  `af/TaskRevision@1` is unchanged, so a Task without bindings keeps the revision, plan and
  `--json` documents it has today.

## Where it lives

Shipped by package L3b. It changed no contract, gate, budget or sandbox boundary; every refusal
it added is new, and every existing one stays.

### Crates and types

| Crate | What it owns |
|---|---|
| `review-core` | `task::input_bindings`: `TASK_INPUT_BINDINGS_V1 = "af/TaskInputBindings@1"`, `TaskInputBindingsV1 { schema, bindings: BTreeMap<String, TaskInputBindingV1> }`, `TaskInputBindingV1 { artifact_id, resolved_artifact_id, snapshot_id, rerooted_snapshot_id, task }`, `ReferencedTaskV1 { task_id, task_revision_id, result_id, port, acceptance, domain_conclusion }`, each `deny_unknown_fields` with a `validate()` in the shape of `ArtifactInputV1::validate`. `resolved_artifact_id` is present only for a re-rooted `source`, whose port carries a new `af/SourceTree@1` envelope. No existing type gained a field. |
| `review-source-git` | `task::TaskSourceOriginV2 { schema, repository_id, content_digest, bound_from }` and `TaskSourceBoundFromV1 { artifact_id, snapshot_id, task }`, both `deny_unknown_fields`, plus `read_origin`, which accepts either generation, branches on `schema` and reports whether a committed `source_revision` is present. `capture_snapshot(cas, manifest, origin, None)` performs the re-rooting; there is no new capture path. |
| `af` | `task_execution::input_bindings` owns the two Task-file reference forms, resolution and the typed record; its `RecordedTasks` seam is the whole Store surface resolution reads. `TaskFile` gained `inputs: Option<BTreeMap<String, TaskInputRefV1>>` under the existing `present_option` treatment, so an absent table stays absent on round-trip. `start_captured` resolves bindings before it builds the revision, skips the construction a bound port replaces, and installs bound ports last so a profile that rebuilds its own input set cannot drop one. `preview::render` annotates `IN` and adds `BOUND` rows through the existing `text()` sanitizer. `present_with_format` adds `input_bindings` and the `af/task-inspection@12` bump beside the existing conditional generations. `self_optimizer::optimization_adapters` admits `af/task-inspection@12`. `task::delivery_common` reads either origin generation and `task::deliver` refuses a re-rooted source before the prepared record and any Git mutation. |

`selection::assess` preserves exactly one record no port carries — the `af/TaskInputBindings@1`
one — instead of overwriting `provenance.input_artifact_ids` with the port identities alone. A
Task without bindings keeps byte for byte the list it had, whatever else its adapter recorded
beside its ports. A source refresh keeps that record too, replacing only the requirements
artifact; the adapter and `review_store::store::task::revision_provenance_input_artifact_ids`,
which the Store's refresh validator uses, derive the same list, so they cannot disagree.

`task::delivery_common` reads the refusal's label from the *current* revision's binding record,
falling back to the origin's `bound_from` only when the revision records no binding: a parentless
generation-2 Snapshot is carried verbatim when it is re-bound, so its origin still names the Task
that re-rooted it first, which is not what this Task's file referenced.

### Display

- `af task explain` writes `IN    history <- task chain-base/history, requirements, source`, and
  `--tree` adds, per bound port, `BOUND <port> <- task <id>/<port> (<acceptance>/<conclusion>)`
  followed by the exact artifact ID on a row of its own — and, for a re-rooted `source`, a
  `re-rooted <id>` row. An exact-artifact binding reads `artifact` in place of the Task and port
  and `-` for the acceptance and the conclusion it has none of: the preview sanitizer maps every
  non-ASCII character to `?`, so the row uses an ASCII hyphen rather than an em dash.
- `af task show` prints `bound <port> <- task <id>/<port> (<acceptance>)` or
  `bound <port> <- artifact sha256:…`, through the same sanitizer.

### Schemas

`schemas/task-file-v1.json` gained the optional `inputs` property with the two closed forms;
`schemas/task-input-bindings-v1.json`, `schemas/task-source-origin-v2.json` and
`schemas/task-inspection-v11.json` gains the optional `input_bindings` property. The `SCHEMAS` array in
`crates/review-core/tests/schema_parity.rs` and its length constant grew by three, with parity
tests in `crates/review-core/tests/schema_parity/task_contracts.rs` beside the existing
Task-contract ones.

### Tests

- Unit, `review-core`: `TaskInputBindingsV1` rejects a non-digest artifact ID, an unknown field
  in any of the three closed shapes, an empty bindings map and a re-rooted binding that lost the
  Snapshot it came from; a binding with no `task` round-trips and writes no absent member.
- Unit, `review-source-git`: generation 1 and generation 2 origins both read, including the
  explicit `"source_revision": null` a dirty capture has always written; generation 2 reports no
  committed revision; an unknown field or an unknown generation is refused in either.
- Unit, `af::task_execution::input_bindings`: a binding on `requirements`, `base`, `continuation`
  or any other port is refused by name; a reference to an unknown Task, to a Task that is not
  finished, to a port the result does not carry, and to an artifact whose type does not match the
  port each refuse with a message naming the Task and the port; a `many` output bound into a
  `one` port refuses whether it holds one artifact or several; an exact-artifact `history`
  binding resolves; a root capture is carried verbatim and a derived tree is re-rooted over the
  identical Manifest; a parentless generation-2 source is carried verbatim; an exact-artifact
  source whose envelope names one Snapshot in its subject and another in its payload is refused,
  while the honest envelope for the same root capture resolves; every refusal is ASCII and
  control-free for newline, escape, bidi and non-ASCII values in both reference forms and in the
  map key; and a Task file with no `inputs` table round-trips byte for byte.
- Unit, `af::task_execution::preview`: the `IN` annotation and the `BOUND` rows sanitize a
  hostile Task ID, as `display_data_cannot_inject_terminal_actions` already requires, render the
  exact-artifact form, and keep the exact artifact ID off the wrap boundary.
- Unit, `af::self_optimizer::optimization_adapters`: a real bound `af/task-inspection@11` receipt is
  admitted and its accounting reads exactly as the same receipt at `@11`.
- Integration, `crates/af/tests/task_input_bindings.rs`: the `fixtures/task-runtime/bound-inputs/`
  repository runs four Tasks in one Store. `chain-base` is an ordinary reviewed implementation
  whose Pipeline also exposes the embedded Review's `history`. `chain-history` binds `history` to
  that output and runs `fixture/plain`, which hands the ledger to its implementer; it plans, runs
  and delivers, proving the ledger crossed without a file. `chain-source` binds `source` to
  `chain-base`'s `snapshot`, plans and runs, and its `af task deliver` refuses naming the
  referenced Task, leaving no branch, no worktree and no prepared record. `chain-rebound` binds
  that re-rooted tree again by exact artifact: it is carried verbatim, and its delivery refusal
  names the artifact it bound rather than the Task recorded in the origin of the first
  re-rooting. The test asserts the `af task explain` `BOUND` rows, validates both bound documents
  against `schemas/task-inspection-v11.json`, and checks that `chain-base`'s own document is
  unchanged.
- Integration, the same file: `successor-issue.json` binds `history` and captures a local issue.
  Changing the issue and running `af task refresh` makes a new revision that still carries the
  binding record in its provenance, still emits `input_bindings` in its `af/task-inspection@11` document, and
  still prints the `BOUND` row and the `bound` line.
- Unit, `af::providers`: a Claude usage probe whose leader starts a same-group descendant that
  ignores `SIGHUP` and then exits without a limit leaves no descendant behind — the exit is
  observed without reaping and the group is killed before the leader is waited for.
- Compatibility: the existing Task-runtime fixtures and `crates/af/tests/task_public_schemas.rs`
  pass unedited, which is the evidence that a Task file without `inputs` is unaffected.

## Widened by package R4

[ADR-0134](../adr/0134-bind-any-declared-root-port-to-recorded-task-outputs.md) changed no
contract beyond one optional member, and every refusal ADR-0117 made stays.

| Crate | What changed |
|---|---|
| `review-core` | `TaskInputBindingV1` gained `also: Vec<TaskInputBindingV1>` — absent when empty, an explicit empty list refused — whose items name a Task, are never re-rooted and never nested, at most `MAX_BOUND_OUTPUTS - 1` of them. |
| `af` | `task_execution::input_bindings`: `TaskInputRefV1::Outputs` is the list form, read by JSON shape so no form is parsed from another's spelling; `DeclaredPorts` is the selected Pipeline's root inputs at resolution; `resolve` takes them, binds any declared port from a result output, refuses exact artifacts beyond ADR-0117's ports, and gathers a list into a `many` port. `start_captured` builds `DeclaredPorts` from the captured catalog. `preview` and `af task show` print one row per bound output. The `builtin/report` starter's generated schemas admit a `snapshot_id` on `comparison` and `measurements` values. |
| `review-store` | `validate_bound_input_refs` judges input ports and the root inputs receipt: a `many` port that names no Snapshot claims none for its artifacts. Output ports keep `validate_input_refs`. |
| `review-pipeline` | `typed_inputs` lets a `many` input the operator declares `unbound` span Snapshots and then name none. |
| `review-runner` | The Worker renderer shows each value of such a port with its own Snapshot. |

`schemas/task-file-v1.json` gained the list form and `schemas/task-input-bindings-v1.json` the
`also` list (`furtherOutput`); the parity test in
`crates/review-core/tests/schema_parity/task_contracts.rs` covers a port bound from two outputs
and refuses a re-rooted, nested, anonymous or empty `also`.

Tests:

- Unit, `review-core`: a port bound from several outputs round-trips, writes no `also` for one
  output, and refuses an anonymous first or further output, a republished or re-rooted one, a
  nested one, a non-digest one, too many, and an explicit empty list.
- Unit, `af::task_execution::input_bindings`: `requirements`, `base` and `continuation` stay
  refused even where declared; an undeclared port names the selected Pipeline, and without one
  names the kind or asks for the Pipeline when declarations differ; a `raw_artifact_ids` or
  `runtime_evidence` name and an exact artifact on a declared port are refused with the
  result-outputs message; a declared `one` port keeps its output's Snapshot and a mismatch names
  both types; a `one` output into a `many` port points at the list form; two Measurements of two
  Snapshots bind with no port Snapshot and one further output in the record, one listed output
  keeps its Snapshot; a list into a `one` port, a mistyped, repeated or unrecorded listed output
  is refused; and the Task-file list form round-trips while every malformed spelling is refused.
- Unit, `af::task_execution::preview`: a port bound from two outputs annotates `IN` with both
  and gets one `BOUND` group per output.
- Integration, `crates/af/tests/it/task_research_chain.rs`: one repository carries the shipped
  experiment, report and Document starters, and one Store holds three Tasks. The experiment runs
  to `verified`; a report Task binding `comparison`, `measurements` (from `baseline` and
  `candidate`) and `source` to it plans with the `BOUND` rows, runs to `verified` in three
  command Attempts, records `measurements` with no Snapshot while each Measurement keeps its own,
  and the author's and the verifier's context manifests name the exact comparison and both
  Measurements; a report Task binding `sources` to the Document Task's `document` is refused at
  plan time naming `af/Document@1` and `af/ReportSources@1`, and nothing is recorded. A second
  test drives every other refusal above through `af task plan`.
- Compatibility: `task_input_bindings.rs`, `task_report.rs`, `task_experiment.rs`,
  `task_public_schemas.rs` and the other Task fixtures pass unedited, which is the evidence that
  a Task file without `inputs`, or with ADR-0117's bindings, is unaffected.
