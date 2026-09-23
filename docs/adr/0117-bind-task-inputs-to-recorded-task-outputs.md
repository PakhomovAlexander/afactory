# ADR-0117: Bind a Task input port to a recorded Task's output

Status: accepted, 2026-09-23.

## Context

[ADR-0115](0115-declare-the-af-layout-and-keep-task-files-out-of-git.md) declared what `.af/` may
hold and [ADR-0116](0116-report-undeclared-af-paths-and-let-a-project-refuse-them.md) reports what
a Snapshot carries under it that the layout does not name. Both treat the symptom. The cause is
the third item in the problem statement of
[`docs/design/af-layout-plan.md`](../design/af-layout-plan.md): **a Task file cannot name another
Task's outputs.** Chaining implementation to repair to review meant exporting the predecessor's
candidate tree and reviewer results to files, committing them so the next capture would see them,
and pointing the next Task file at the paths. Every later Snapshot then carried them, which is how
one consumer's pull request reached 244,008 added lines.

The Task-file adapter builds every root input itself and offers no other way in
(`crates/af/src/task_execution.rs`, `start_captured`):

- `requirements` from the mandatory `goal`, or from a captured Issue;
- `source` from a fresh Git capture of the invoking checkout, published as
  `af/SourceTree@1` over an `af.task-snapshot/1` whose `origin_id` is an
  `af.task-source-origin/1` record naming the repository, the commit and the content digest;
- `sources` and the optimization `history` from a *project-relative path* — a file that must be
  inside the captured tree, which is precisely the export-to-disk habit;
- the review `history` from `normalize_root_inputs`, which fills a port declaring
  `root_default = "empty_review_history"` with a typed empty `af/ReviewHistory@1`
  (`crates/review-config/src/task/catalog.rs`).

Three existing rules constrain any answer, and none of them may be relaxed:

- The compiler already checks every root input against the root Pipeline contract for exact
  `artifact_type` and `cardinality` equality before it builds a single node
  (`crates/review-graph/src/task.rs`: "Root input {name} has an incompatible type or
  cardinality"), and `normalize_root_inputs` already refuses an input the contract does not
  declare.
- Delivery under [ADR-0031](0031-deliver-verified-tasks-to-new-local-worktrees.md) compares the
  target repository against the Task's own source Snapshot exactly: the source Snapshot must be a
  root capture (`parent_snapshot_id` absent), its origin's `content_digest` must equal it, its
  `source_revision` must equal the target's `HEAD` commit, and the committed and dirty captures of
  the target must reproduce its Manifest byte for byte
  (`crates/af/src/task/delivery_common.rs`, `crates/af/src/task.rs::verify_source_authority`).
- A Snapshot's identity is content **and** capture provenance. Re-publishing a Snapshot record is
  allowed; silently changing what an existing Snapshot ID means is not.

## Options

- **Export the referenced artifact to a file and name the path.** This is the only thing a
  coordinator can do today, and it is what this ADR exists to end — the kernel wrote none of
  those files. Rejected: the exported bytes are only reachable by the
  next capture if they sit in the checkout, so an artifact that the Store already holds by exact
  identity is copied into every subsequent Snapshot, every candidate and every delivered worktree;
  and the path names no identity, so two runs of the same Task file can resolve different bytes.
- **A `--continue-from TASK` flag on `af task plan` / `af task start`.** Rejected on three counts.
  It puts a durable part of the request on the command line, where nothing captures it, so the
  Task revision no longer states its own inputs; it can only mean one thing, so it cannot say
  *which* port receives *which* output, and it would have to guess `snapshot` to `source` and
  `history` to `history`; and a flag is invisible to `af task export`, which carries Task
  definitions between machines ([ADR-0057](0057-export-portable-task-definitions-without-execution-authority.md)).
- **Cross-Store references — `{ "store": "…", "task": "…" }`.** Rejected. The Store is the
  kernel's replay boundary: a Ledger is a function of one event log and the immutable artifacts
  its events reference, and a reference into another Store would make the new Task's plan
  unreplayable the moment that Store moved, or let a path outside `--state` decide what a Worker
  reads. Moving work between machines already has an answer that carries bytes rather than a
  path: `af task export`.
- **Reference an Attempt's output instead of a published `af/TaskResult@1`.** Rejected: an
  Attempt's outputs are provisional until the Task's result is constructed. An abandoned Attempt
  is fenced and anything arriving under a revoked epoch is quarantined, so a reference could name
  bytes the kernel has already decided are not this Task's answer.
- **Require the referenced Task's acceptance to be `satisfied`.** Rejected; see the decision.
  The cases that most need a reference are exactly the unsatisfied ones — repair the candidate a
  reviewed implementation left `changes_requested`, verify an evaluator's rejected Snapshot — and
  refusing them would preserve the export-to-disk habit for the only chain anyone runs twice.
- **Let a binding supply `requirements`.** Rejected. `goal` is mandatory in the Task file and the
  adapter always turns it into the `af/Requirements@1` the Task is judged against; a binding on
  that port would let a prior Task's requirements silently replace the stated goal, and the goal
  printed by `af task explain` would no longer be the one the acceptance obligation reads.
- **Carry the referenced `af/SourceTree@1` verbatim into the `source` port in every case.**
  Rejected for the interesting case. A referenced `snapshot` output names a *derived* Snapshot,
  whose `parent_snapshot_id` is set and whose origin's `content_digest` is the predecessor's S0,
  so delivery would have to stop requiring a root source Snapshot and stop comparing the origin
  digest — weakening the one gate that makes a delivered worktree provably the verified result.
  Kept for a referenced Snapshot that is already a root capture, where verbatim is exactly right.
- **Store the binding inside `af/TaskRevision@1`.** Rejected. The revision is a versioned wire
  contract with a schema, parity tests and fixtures; a new field means `af/TaskRevision@2` and a
  migration for every reader, to record something that is provenance rather than execution input.
- **Resolve the reference lazily, at run or resume time.** Rejected. The compiled plan would no
  longer state what the Task reads, a resume would have to re-read the Task file the kernel
  deliberately captured, and a plan a developer signed could resolve to different bytes the
  second time ([ADR-0056](0056-share-planning-accounting-and-authenticate-generated-plan-decisions.md)).
- **An optional `inputs` table in the Task file, resolved once at plan time into exact artifact
  IDs.** Chosen.

## Decision

### The Task-file shape

`af.task-file/1` gains one optional top-level table, `inputs`, mapping a **root input port name**
to exactly one of two forms:

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
  "facts": { "standard": true },
  "limits": { "tokens": 900000, "max_attempts": 4, "wall_ms": 7200000,
              "verification": { "tokens": 300000, "attempts": 2, "wall_ms": 1800000 } }
}
```

- `{ "task": "<task_id>", "port": "<output port>" }` names an output port of a Task recorded in
  the same Store. Both members are required; no other member is accepted.
- `{ "artifact": "sha256:…" }` names one artifact in the same Store by exact ID, for a caller who
  already holds the identity — a re-plan of a Task whose predecessor was pruned from projection,
  or a fixture that pins bytes. No other member is accepted.

`inputs` is absent by default and `deny_unknown_fields` still applies to the two forms and to the
file. A Task file written before this decision parses and compiles to the identical revision, so
every existing fixture and recorded plan is byte-identical.

### Which root ports may be bound

Exactly the ports whose construction the Task-file adapter owns: **`source`**, **`history`** and
**`sources`**. A binding *replaces* that construction for its port, and nothing else changes: a
bound `source` performs no Git capture of the invoking checkout, a bound `history` suppresses the
`empty_review_history` root default, and a bound `sources` makes `document_sources` unnecessary
(supplying both is an error, because two constructions would claim one port).

`requirements` is not bindable, for the reason given above. `base` and `continuation` are not
bindable either: the adapter constructs neither, and a Task file that could set `base` would turn
an implementation Task's Subject into a diff without any of the Review-selector rules of
[ADR-0041](0041-make-review-selectors-explicit-and-refuse-empty-diffs.md). A Pipeline that needs
them keeps declaring them optional and binds them inside its own graph. A binding naming any
other port — including one a Pipeline does declare — is refused by name at plan time, before the
existing "Task binds an undeclared root input" refusal ever runs.

### Resolution happens at plan time, once, from the `--state` Store

`af task plan` and `af task start` resolve every binding while building the Task revision, reading
only the Store named by `--state`. The result is an ordinary `ArtifactInputV1` in
`TaskRevisionV1.inputs`: exact `artifact_ids`, the exact `artifact_type`, the declared
`cardinality` and, for `source`, the exact `snapshot_id`. From that point the binding is
indistinguishable from a capture: the compiled plan and its `af.compiled-task/1` graph carry only
artifact IDs, `af task run`, resume, retry and replay read the recorded revision, and **the
referencing Task file is never read again**. A developer's signed approval therefore binds the
exact bytes, and re-planning with an edited `inputs` table produces a new revision and invalidates
that approval, exactly as any other Task-file edit does.

### Type and cardinality

Two checks, both before any Worker or Provider admission:

1. At resolution, the adapter compares the referenced port's recorded `artifact_type` with the
   type that port carries for this Task's profile — `af/SourceTree@1` for `source`,
   `af/ReviewHistory@1` for a review `history` and `af/OptimizationHistory@1` for an optimization
   one, `af/DocumentSources@1` for `sources` — and refuses a mismatch naming both types, the
   referenced Task and the referenced port. Cardinality is compared the same way, as exact
   equality of the referenced port's recorded `cardinality` with the destination port's: the
   binding preserves the recorded cardinality, never recasts it, and a `many` output bound into a
   `one` port is refused whether it currently holds one artifact or several. A
   `{ "artifact": … }` reference is cardinality `one` by definition.
2. At compilation, the existing root-input check in `crates/review-graph/src/task.rs` compares the
   resolved port with the selected Pipeline's declared contract for exact type and cardinality
   equality. This check is not bypassed, extended or duplicated by the binding; it remains the
   one that cannot be talked around, and it runs before the graph exists, let alone a dispatch.

For a `{ "artifact": … }` binding the first check reads the artifact envelope's own type. Both
refusals are ordinary Task-file input errors: `af: …` on stderr, exit 1, and the `af/error@1`
document under `--json`.

### What the referenced Task must be

The referenced Task must be **recorded in this Store**, its phase must be
`Finished { result_id }`, that `af/TaskResult@1` must read and validate, the named port must exist
in `result.outputs`, and every artifact it names must be present and verifiable in the CAS. A Task
that is planned, running, waiting or absent is refused by that name; so is a port the result does
not carry, listing the ports it does.

**An unverified Task's `snapshot` may be referenced.** The reference is a statement about bytes,
not about quality, and three facts make it safe. The output already passed the executor's own
output-contract admission to reach `result.outputs`, which is the entire claim a reference makes.
Nothing crosses (below), so the new Task earns its own acceptance regardless of what the
predecessor's was. And the alternative would leave the export-to-disk habit in place for the
chains that need referencing most: repairing a candidate a reviewer rejected, or re-verifying a
Snapshot an evaluator refused. The referenced Task's `acceptance` and `domain_conclusion` are
copied into the binding record and printed by `af task explain`, so an operator approving the plan
sees `unsatisfied / changes_requested` before anything runs, rather than discovering it later.

### `source` is re-rooted, and `af.task-source-origin/2` names where it came from

A bound `source` is decided by one property of the referenced Snapshot, and nothing else:

- **Already a root capture** — `parent_snapshot_id` absent and a generation-1 origin, which is
  what a Task's own `source` port holds. The referenced `af/SourceTree@1` is carried verbatim: it
  is byte-for-byte what a fresh capture of that commit would have produced, it keeps its
  `source_revision`, and the resulting Task delivers exactly as one planned from the checkout.
- **Derived** — `parent_snapshot_id` set, which is what a `snapshot` output holds. It cannot be
  carried verbatim, because delivery requires a root source Snapshot whose origin digest equals
  it. The adapter republishes the referenced **Manifest** — the same bytes, the same content
  digest, the same manifest artifact, since the CAS is content-addressed — as a new root
  `af.task-snapshot/1` with no `parent_snapshot_id` and a new origin record:

  ```json
  {
    "schema": "af.task-source-origin/2",
    "repository_id": "<copied from the referenced Snapshot's own origin>",
    "content_digest": "<the bound tree's content digest>",
    "bound_from": {
      "artifact_id": "sha256:…",
      "snapshot_id": "sha256:…",
      "task": {
        "task_id": "layout-l3b",
        "task_revision_id": "sha256:…",
        "result_id": "sha256:…",
        "port": "snapshot",
        "acceptance": "unsatisfied",
        "domain_conclusion": "changes_requested"
      }
    }
  }
  ```

Generation 2 is `deny_unknown_fields`, carries `bound_from.task` only for a `{ "task", "port" }`
binding, and — deliberately — **has no top-level `source_revision`**. Generation 1 is written
unchanged for every ordinary capture, so no existing record, receipt or fixture moves; readers
accept both and branch on `schema`.

The omission is the point. `repository_id` is copied forward so the tree still states which
repository it belongs to, and `content_digest` describes the tree the record actually describes,
so delivery's origin check passes. But a derived tree has no commit, and inventing one — reusing
the predecessor's `source_revision`, which names a *different* tree — would let
`verify_source_authority` compare the target repository against the wrong Snapshot. With the
field absent, the existing rule "delivery requires a committed source Snapshot" fires on its own,
and `af task deliver` adds one sentence naming the referenced Task and port and saying that the
tree must exist as a commit before a Task over it can be delivered.

Re-rooting also publishes a new `af/SourceTree@1` envelope for the re-rooted Snapshot, whose
payload and `subject_snapshot_id` name that Snapshot, because the referenced envelope still names
the derived one and `source_snapshot` admission refuses a SourceTree that disagrees with its
Snapshot. The resolved `ArtifactInputV1` therefore carries the *new* artifact ID together with
the re-rooted `snapshot_id`, one consistent pair, and the binding record keeps both the
referenced and the resolved SourceTree artifact IDs.

A parentless Snapshot whose origin is already generation 2 — a re-rooted source referenced again,
for example through `{ "artifact": … }` — is carried verbatim as the root it already is: nothing
is re-rooted twice, its generation-2 origin stays, and delivery of the resulting Task refuses
exactly as it did for the first re-rooting, because there is still no committed revision.

Only a derived `source` is re-rooted, and generation 2 exists for exactly that case. `history` and
`sources` always carry the referenced artifact ID verbatim into the port, because they hold no
Snapshot semantics that sealing, ancestry or delivery depend on.

### Authority: the reference carries provenance only

Nothing else crosses a Task boundary, and each of these is stated so that no future reader has to
infer it:

- **No acceptance.** The referenced Task's acceptance state is display data in the binding
  record. The new Task's acceptance obligations are its own and are evaluated against its own
  evidence.
- **No verification.** A referenced verified Snapshot does not make the new Task's derived
  Snapshot verified; the new Task runs its own checks, Review and evaluation.
- **No plan approval.** A signed `af/PlanDecision@1` binds one exact revision, plan and authority
  ([ADR-0056](0056-share-planning-accounting-and-authenticate-generated-plan-decisions.md)). A
  reference to an approved Task's output is not an approval of the referencing plan.
- **No delivery authority.** Delivery still requires the new Task's own verified result, its own
  explicit Task-ID confirmation, and the exact comparison of its own source Snapshot against a
  clean target repository that [ADR-0031](0031-deliver-verified-tasks-to-new-local-worktrees.md)
  requires. This ADR changes none of those rules; it only makes one of them refuse earlier and
  more clearly.
- **No budget and no policy.** The referenced Task's spend stays on its own ledger, and the new
  Task resolves its own Authority Snapshot, project policy and Provider admissions.

### Display and the record

The binding is recorded as one typed artifact, `af/TaskInputBindings@1`, referenced from
`TaskRevisionV1.provenance.input_artifact_ids` — the same place ADR-0116 put
`af/UndeclaredAfPaths@1` when there was no port to hang an observation on. It lists, per bound
port, the referenced `task_id`, `task_revision_id`, `result_id`, output port, `acceptance`,
`domain_conclusion`, the referenced `artifact_id` and, for `source`, both the referenced and the
re-rooted `snapshot_id`. It is written only when the Task file carries an `inputs` table, so a
Task without bindings keeps the exact revision identity it has today. `af/TaskRevision@1` is
unchanged.

`af task explain` annotates the existing `IN` line — `source <- task layout-l3b/snapshot` for a
Task reference, `source <- artifact sha256:…` for an exact-artifact one — and `--tree` adds one
`BOUND` row per port carrying the referenced Task, port, acceptance, domain conclusion and exact
artifact ID; for an exact-artifact binding the row reads `artifact` in place of the Task and port,
and `—` for acceptance and conclusion, since no Task was named. Both go through the preview's
existing `text()` sanitizer, because a Task ID is untrusted display data, and the sanitizer tests
cover both forms.

`af task show` gains an `input_bindings` field in the `af/task-inspection@11` document, present
only when there is a binding; the document keeps its one generation, and a Task without bindings
emits exactly the document it always did. Its plain-text output prints one
`bound <port> <- task <id>/<port> (<acceptance>)` line per Task binding and one
`bound <port> <- artifact sha256:…` line per exact-artifact binding. The delivery refusal for a
re-rooted source names the referenced Task and port when the binding recorded one, and otherwise
the referenced artifact ID: "source was bound to artifact sha256:…; the tree has no commit".

The self-optimizer's AF history adapter reads that same `@11` receipt; an `input_bindings` record
is provenance and changes no counter, which a test over a real bound receipt proves.

## Consequences

A chain of Tasks becomes a chain of artifact IDs in one Store. Nothing is exported, nothing is
committed to make the next capture see it, and the `.af/tasks/<workstream>/` habit that ADR-0115
and ADR-0116 describe loses its last justification. Because resolution happens once, a plan states
its inputs completely, and resume, retry, replay and `af task export` need no access to the
referencing Task file or to the predecessor's projection.

The honest cost is delivery. A Task whose `source` is bound to a derived tree can be planned, run,
checked, reviewed, evaluated and verified, but cannot be delivered: there is no commit for the
target repository's `HEAD` to equal, so ADR-0031's exact comparison has nothing to compare
against. The refusal is explicit and names the referenced Task. An operator who wants delivery
commits the tree — `af task deliver` on the predecessor, then a commit in the new worktree — and
plans the successor from that commit, which is the workflow `docs/design/af-layout-plan.md` §3
already describes. This makes the implementing package L3b's fixture two bound Tasks over one
predecessor rather than a single Task that plans, runs and delivers with `source` bound: one
binds `history` and delivers, the other binds `source` and asserts the refusal.

Referencing an unverified Task's output means an operator can build on a candidate a reviewer
rejected. That is intended, and it is visible: `af task explain` prints the referenced acceptance
beside the port before the plan is approved, and the resulting Task still has to earn every
obligation of its own.

Old records stay readable. `af.task-source-origin/1` is still written for every ordinary capture
and is still the only generation any existing Snapshot carries; `af/TaskRevision@1` gains no
field; a Task file without `inputs` produces byte-identical revisions, plans and `--json`
documents. Nothing in this decision removes a path, relaxes a type check, or lets a Task read
bytes the Store does not already hold.
