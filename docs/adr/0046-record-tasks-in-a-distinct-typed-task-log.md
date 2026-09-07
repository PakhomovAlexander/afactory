# Record Tasks in a distinct typed Task log

**Status:** accepted (2026-09-07)

`af task` — minimal v2 sequential implementation
([ADR-0030](0030-complete-minimal-v1-and-v2-before-dogfood.md)) and its local delivery slice
([ADR-0031](0031-deliver-verified-tasks-to-new-local-worktrees.md)) — grew beside the Campaign
log rather than inside it, and no decision said what its log guaranteed. The whole-tree audit of
`b604b4a` found what that silence cost: five event types written as bare string literals, absent
from `review_core::EventType` and from `schemas/`; a `TaskStore::append` that never checked the
artifact it referenced existed; the complete sandbox mutation list inlined into the evaluator
prompt; a derived Snapshot with no ceiling, so one `cargo check` inside the implementer sandbox
would file ~10k build files and ~1 GiB into the CAS as permanent state; the derived tree
verified serially, once per entry, and re-materialized once per Gate and again for the evaluator;
and the capture → sandbox → Gate → implementer → evaluator composition wired by hand in the CLI,
outside the crate the README names as the only composition layer. M0's rule — no feature adds
another bare event literal — was being honoured by the Campaign log and not by the Task log.

The Task log is a **distinct contract**: not a second kernel, and not a tenant of the Campaign
`EventStore`. It carries these guarantees.

1. **A closed vocabulary.** `review_core::event::TaskEventType` lists exactly nine event types
   (`TaskOpened@1`, `WorkerCompleted@1`, `SnapshotDerived@1`, `GateCompleted@1`,
   `EvaluationCompleted@1`, `TaskCompleted@1`, `TaskDeliveryPrepared@1`, `TaskDelivered@1`,
   `TaskDeliveryFailed@1`); `schemas/task-event-v1.json` lists the same nine and a parity test
   keeps the two identical. A row whose type is outside the vocabulary fails closed when read.
2. **Artifact-referencing events.** A Task event carries no inline payload. It names exactly one
   immutable CAS artifact, and that artifact's `schema` marker is the payload contract
   (`schemas/task-*.json`; `GateCompleted@1` references a `CheckResult@1`). The vocabulary
   declares which marker each type references (`TaskEventType::artifact_schema`), so a type
   cannot silently point at the wrong record.
3. **Publication order.** `TaskStore::append` admits an event only after
   `review_pipeline::task::admit_task_artifact` has re-verified the artifact through the same
   CAS operations `EventStore::append` uses (`prepare_for_publication`,
   `get_json_for_publication`), checked the marker the event type declares, and the CAS has been
   flushed. A Task row can follow durable bytes, never precede them — the ordering the design
   demands of every log.
4. **One composition, in the composition layer.** `review_pipeline::task::TaskKernel` runs
   everything from `TaskOpened@1` to `TaskCompleted@1`: the implementer sandbox cloned from one
   source template, the seal, the ceiling, the derived Snapshot and its verification, every Gate
   and the evaluator as copy-on-write clones of one derived template, the budget checks, and the
   typed outcome. The CLI keeps argument parsing, state resolution, authority loading from the
   captured source Snapshot, the SQLite log, and presentation — the split `af review run` already
   has with `Kernel::from_loaded`. The kernel writes through a one-method `TaskLog` trait because
   `review-pipeline` deliberately does not depend on SQLite.
5. **Bounded Worker context.** The evaluator receives `mutation_summary` — per-kind counts, a
   sorted sample of twenty paths, and the `af/derived-snapshot@1` artifact ID — never the complete
   path lists ([ADR-0028](0028-prioritize-wise-token-use-and-minimum-worker-context.md)). The full
   lists are durable once, in that artifact. One helper (`review_pipeline::mutations`) serves the
   review path's provenance record and the Task path's prompt. Least-sufficient is not
   least-informed: the evaluator's prompt (`.af/workers/evaluator/reviewer.md`) names the exact
   `af/evaluate-input@2` fields it receives, says that `sample` is capped and `truncated` means
   partial, and directs the Worker to read its sandbox and treat the counts — not the sample — as
   the scope signal.
6. **A ceiling, checked before publication.** `DerivedSize::measure` reads only the seal scan's
   metadata. `MAX_DERIVED_MUTATION_ENTRIES_V1 = 4096` and `MAX_DERIVED_MUTATION_BYTES_V1 =
   256 MiB` bound what the implementer added or modified: a `cargo check` of this workspace
   (~10k files, ~1 GiB) does not fit; a change touching every file of this repository plus a
   generated corpus does. `MAX_DERIVED_SNAPSHOT_ENTRIES_V1 = 250 000` and
   `MAX_DERIVED_SNAPSHOT_BYTES_V1 = 4 GiB` bound the whole tree, the numbers a Cache Snapshot is
   held to. Exceeding any of them is a typed `unverified` outcome at stage `snapshot` whose reason
   names the limit; no byte of the refused tree reaches the CAS. The measured size is recorded in
   `af/derived-snapshot@1` and as `derived_snapshot_size` in `af/task-outcome@1`.
7. **One verification pass, no repeated work.** `verify_snapshot` checks each distinct content
   digest once, on the shared bounded executor; the derived tree is materialized into one template
   and cloned per Gate and for the evaluator.

Task records follow [ADR-0002](0002-event-payload-changes-bump-the-type-version.md): a shape
change bumps the version. `af task` shipped in `v0.7.1`, so real Task state exists in the
shapes that release wrote, and `af/…@1` readers are permanent. What this decision adds to `@1`
is therefore **optional, never required**: the `schema` markers on `af/worker-evidence@1` and
`af/task-evaluation@1`, `size` on `af/derived-snapshot@1`, and `derived_snapshot_size` on
`af/task-outcome@1` are written by every current record and absent from every `v0.7.1` one, and
`ignored_paths` on `af/task-delivery@1` predates even that. A `required` entry is not an
additive change — it would make durable state invalid against the schema that names its own
version — so those five stay out of the schemas' `required` lists while keeping their `const`,
`pattern`, and type constraints. That the current binary always writes them is a test assertion
(`crates/reviewctl/tests/task_contracts.rs`), not a schema rule, and the `v0.7.1` shapes are
stored as fixtures the `@1` schemas must keep accepting
(`crates/reviewctl/tests/fixtures/task-v0.7.1`).

Three consequences of that rule are fixed here so they are not rediscovered:

- **The `stage` vocabulary is closed inside `@1`.** `af/task-outcome@1`'s `outcome.stage` is an
  enum under `additionalProperties: false`; the `snapshot` member is safe only because the schema
  was born containing it. Adding a stage bumps the type to `af/task-outcome@2` — a reader holding
  today's schema file must not start hard-failing records it was told it could read. An open enum
  with a fallback was rejected: an unrecognised stage is exactly the case where the reader cannot
  act correctly, so silence is worse than a version it can refuse by name.
- **Every declared `af/…@N` marker has a schema.** A marker with no schema file is a decorative
  version number: nothing states what the version means, and renumbering it breaks no test. The
  Worker-input markers now have theirs — `schemas/task-worker-package-v1.json`,
  `schemas/task-implement-input-v1.json`, `schemas/task-evaluate-input-v2.json` — and a test
  enumerates the marker constants declared in the workspace and fails when one is unregistered.
  `af/evaluate-input@2` is a real bump: `mutations.added` went from an array of paths to an
  integer count.
- **A serialized Task event is the whole envelope.** `schemas/task-event-v1.json` requires
  `task_id`, so the rows `af task show --json` emits as `history[]` carry it, rather than being a
  subset of the envelope the schema describes.

## Considered options

- **Route Task events through `review_store::EventStore` keyed by Task ID.** One log, one
  admission path, one schema file. Rejected. A `RunEvent@1` carries an inline validated payload,
  wall-clock `occurred_at`, node and Attempt lineage, and a first-event `CampaignOpened@1`
  transition that replay enforces; a Task record is an artifact-referencing receipt with none of
  those, and the Ledger projection would have nothing to project. Adding nine variants to
  `EventType`, nine `validate_event_payload` arms, nine replay arms, and nine `run-event-v1.json`
  branches would make the Campaign contract carry a vocabulary its readers must ignore, and every
  Campaign fixture would gain types that never occur in a Campaign.
- **Keep the literals and document them.** Rejected: it is exactly the state M0 closed for the
  Campaign log, and the audit showed it drifts — the store accepted any string and any artifact ID.
- **Leave the composition in the CLI.** Rejected. `reviewctl` already contained a second wiring
  of checks, sandboxes, and runners with no home for a test that does not spawn the binary; the
  README's claim that `review-pipeline` adds only wiring was false for one of the two commands.
- **Bound the whole derived tree only.** Rejected: a whole-tree ceiling cannot separate a 1 GiB
  `target/` from a large source repository. Bounding what the implementer added or modified is
  what the stated rationale needs; the whole-tree numbers remain as the same protection a Cache
  Snapshot has.
- **Apply capture's ignore semantics to sealed additions,** so build output never enters a derived
  Snapshot at all. Not decided here. It changes what a derived Snapshot *is* — today it is the
  exact sealed tree, and delivery materializes exactly that, ignored paths included — and it
  deserves its own decision. The ceiling makes the current behaviour safe until then.

## Consequences

- Adding a Task event means adding its `TaskEventType` variant, its entry in
  `schemas/task-event-v1.json`, its artifact schema and marker mapping, and the parity test
  catches any of the four missing. The `af/…@1` readers are permanent
  ([ADR-0002](0002-event-payload-changes-bump-the-type-version.md)).
- `crates/reviewctl/tests/task_contracts.rs` validates every row a real Task and delivery persist
  against `schemas/`, and every stored `v0.7.1`-era record against the same `@1` files; a record
  shape can no longer change without a schema change beside it, in either direction. It also
  fetches every artifact a Worker context manifest names and validates it against the schema the
  manifest claims for it, so `artifact_type` is a falsifiable claim rather than a free string.
- The Task ceiling is a kernel constant, not project policy. A project that legitimately needs a
  larger derived Snapshot needs a new decision, not a knob.
- `review-pipeline` gains `serde` and `tempfile` as ordinary dependencies; it still does not
  depend on SQLite, and the Task log implementation stays in `reviewctl`.
- Open question, deliberately left: whether sealed additions should be filtered by the capture's
  ignore semantics before they become part of a derived Snapshot.
