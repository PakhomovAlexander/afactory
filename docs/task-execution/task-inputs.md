# Task input bindings

A Task file can bind a root input port to an output of a Task already recorded in the same Store,
instead of exporting that output to a file and capturing it again. The decision is
[ADR-0117](../adr/0117-bind-task-inputs-to-recorded-task-outputs.md); this page is its
Task-file reference and a map of where it is implemented.

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
| `{ "artifact": "sha256:…" }` | One artifact in the same Store, by exact ID; cardinality `one`. |

Bindable ports are `source`, `history` and `sources` — the ports the Task-file adapter
constructs. A binding replaces that construction: a bound `source` captures no Git tree from the
invoking checkout (so `--uncommitted`, which captures exactly that, is refused together with a
bound `source`), a bound `history` suppresses the `empty_review_history` root default, and a
bound `sources` makes `document_sources` unnecessary. `requirements`, `base` and `continuation`
are not bindable, and any other name is refused by name at plan time.

The referenced Task must be recorded in this Store, finished, and its result must carry the named
port. An unverified Task's `snapshot` may be referenced; the reference carries provenance only.
Type and cardinality are checked against the port twice — once by the adapter against the profile's
expected artifact type, once by the compiler against the selected Pipeline's contract — both
before any Worker or Provider admission. Cardinality is exact equality of the recorded and the
destination cardinality: a `many` output never binds a `one` port, however many artifacts it
currently holds. A refusal is an ordinary Task-file input error: exit 1
with the `af/error@1` document under `--json`.

## What resolution records

Resolution happens at plan time, once, and only from the `--state` Store. The compiled plan holds
exact artifact IDs, so `af task run`, resume, retry and replay never read the referencing Task
file again, and an edited `inputs` table makes a new revision that invalidates any plan approval.

`history` and `sources` carry the referenced artifact ID verbatim. A bound `source` is admitted
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
  `source <- artifact sha256:…` for an exact-artifact binding); `--tree` adds one `BOUND` row per
  port with the referenced Task, port, acceptance, domain conclusion and exact artifact ID, or
  `artifact` and `-` where no Task was named. Every one of those goes through the preview's
  sanitizer, which maps any non-ASCII character to `?`, so the "nothing to show" marker is an
  ASCII hyphen.
- `af task show` prints one `bound <port> <- task <id>/<port> (<acceptance>)` line per Task
  binding and one `bound <port> <- artifact sha256:…` line per exact-artifact binding, and its
  `--json` document carries `input_bindings` — present only when there is a binding, inside the
  one `af/task-inspection@11` generation. The self-optimizer's AF history adapter reads it as
  provenance that changes no counter.
- The durable record is one `af/TaskInputBindings@1` artifact referenced from the revision's
  `provenance.input_artifact_ids`. `af/TaskRevision@1` is unchanged, so a Task without bindings
  keeps the revision, plan and `--json` documents it has today.

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
