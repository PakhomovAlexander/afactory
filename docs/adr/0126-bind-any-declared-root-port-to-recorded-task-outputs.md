# ADR-0126: Bind any declared root port to recorded Task outputs

Status: accepted, 2026-09-28. Amends
[ADR-0117](0117-bind-task-inputs-to-recorded-task-outputs.md): which root ports a Task file may
bind, and how a `many` port is bound from several recorded outputs.

Implements package R4 of [`docs/design/research-pipelines.md`](../design/research-pipelines.md)
under that plan's §2 ("a bound input carries provenance only"), on top of
[ADR-0124](0124-measure-and-compare-source-candidates-in-the-kernel.md) (Measurements and
comparisons) and [ADR-0125](0125-accept-reports-bound-to-an-exact-source-snapshot.md) (the report
profile's optional `comparison` and `measurements` ports).

## Context

ADR-0117 let a Task file bind exactly the three root ports the Task-file adapter constructs —
`source`, `history` and `sources` — and refused every other name, including a port the selected
Pipeline declares. Research is a chain: an experiment Task records `baseline` and `candidate`
Measurements and a `comparison`, and the report Task that explains them declares `comparison`
(`af/MeasurementComparison@1`, `one`) and `measurements` (`af/Measurement@1`, `many`) root
ports. Under ADR-0117 neither can be bound, so a coordinator would have to export the artifacts
to files again — the habit ADR-0117 exists to end.

Four existing facts constrain the answer:

- A Task-file reference names one output of one Task. `measurements` wants two outputs of one
  Task, the `baseline` and the `candidate`, and they measured **two Snapshots**.
- `af/ArtifactInputV1` names at most one Snapshot, and three places hold a port to it: the Store
  refuses a port whose artifacts' envelopes name another Snapshot
  (`crates/review-store/src/store/task.rs`, "Task port Snapshot identity contradicts its artifact
  envelope"), the executor refuses a node input that spans two (`crates/review-pipeline/src/task.rs`,
  "One Task port spans different Snapshots"), and the Worker renderer refuses a value whose
  Snapshot differs from its port's (`crates/review-runner/src/task.rs`). ADR-0125's implementer
  found exactly this and deferred it here.
- Pipeline selection reads the Task revision, and the revision is built from the resolved
  bindings, so "the selected Pipeline" does not yet exist when a binding resolves.
- `{ "artifact": … }` names any artifact in the Store by exact ID, including an Attempt's raw
  artifacts and its runtime evidence, none of which is a Task's answer.

## Options

- **Add `comparison` and `measurements` to ADR-0117's list by name.** Rejected: every future
  port would need another decision, although the selected Pipeline's declaration is already the
  contract the compiler checks exactly. The list form invites a stale list.
- **Let the exact-artifact form bind the new ports.** Rejected. The plan requires that only
  result outputs bind, and an exact ID cannot say whether it names one: a failed Attempt's raw
  `af/Measurement@1` has the right type. ADR-0117's three ports keep the form, unchanged.
- **Give a port bound from several outputs the first output's Snapshot.** Rejected: it states
  that the candidate's Measurement is about the baseline's tree, which is false, and a Worker
  reading it could not tell which Measurement measured which tree.
- **Recast one `one` output into a `many` port in the single form.** Rejected: ADR-0117 never
  recasts a cardinality, and a silent recast is how a `many` output would one day be read as a
  `one`. The list form says it: a list binds a `many` port and nothing else.
- **Record several outputs as `af/TaskInputBindings@2`, or under synthetic map keys.** Rejected.
  A second generation means a second reader in `af task explain`, `af task show`, delivery and
  the self-optimizer's adapter to record one more list, and the plan asks that
  `af/TaskInputBindings@1` record every binding. A synthetic key such as `measurements-2` is a
  valid port name, so a reader could not tell it from a real port.
- **Resolve a declared port's type after selection.** Rejected: selection reads the revision the
  binding builds, so the order is circular, and a mismatch would then surface only as "No
  Pipeline selected", which names neither type.
- **Render no Snapshot for any value of an `unbound` Worker port.** Rejected here: it would change
  what every existing Worker with an `unbound` port receives, far beyond this decision.

## Decision

### Which ports bind

A Task file's `inputs` table may bind **any root input port the selected Pipeline declares**,
except `requirements`, `base` and `continuation`, which stay refused by name for ADR-0117's
reasons: the goal is the requirements the Task is judged against, a bound `base` would make the
Subject a diff outside
[ADR-0041](0041-make-review-selectors-explicit-and-refuse-empty-diffs.md)'s selector rules, and a Pipeline that needs `continuation`
binds it inside its own graph.

`source`, `history` and `sources` are unchanged: a binding replaces the adapter's construction,
the expected type is this profile's (`af/SourceTree@1`, the review or optimization history,
`af/DocumentSources@1` or, for a report, `af/ReportSources@1`), `source` keeps its re-rooting
rules exactly, and `history` and `sources` carry the referenced artifact with no Snapshot.

Any other port's expected type and cardinality are the selected Pipeline's declaration. At
resolution that is the Pipeline the Task file names in `pipeline`, or — when it names none —
every captured Pipeline accepting the Task's kind, which must then declare the port alike. A
port none of them declares is refused ("not a bindable root input port: the selected Pipeline
… does not declare it"); a port they declare differently is refused with "name the Pipeline in
the Task file". The compiler's exact root-input check still compares the Pipeline actually
selected, before any node exists; this decision does not bypass it.

### Only result outputs bind

The newly bindable ports accept only `{ "task", "port" }`, and the named port must be in the
referenced result's `outputs`, as ADR-0117 requires: the Task recorded in this Store, `Finished`,
its `af/TaskResult@1` valid, every artifact verified in the CAS. A name the result does not
carry — `raw_artifact_ids`, `runtime_evidence` or any other record a Task keeps beside its
result — is refused with a message that says only result outputs bind and lists the outputs
there are. An `{ "artifact": … }` reference on one of these ports is refused with a message
saying why: an exact ID may name an Attempt's raw artifact or runtime evidence.

### Forms, types and Snapshots

- **One output** — `{ "task", "port" }`. The recorded type and cardinality must equal the port's
  exactly. A `one` port keeps that output's Snapshot ID; so does a `many` port bound from one
  `many` output. A `one` output into a `many` port is refused as a cardinality mismatch, and the
  refusal names the list form.
- **Several outputs** — a list of one to sixteen `{ "task", "port" }` references, in order,
  binds only a `many` port. Each listed output must carry the port's type and may hold one
  artifact or several; together they form the port's artifacts in list order, which must be
  distinct. One listed output keeps its Snapshot ID. Several carry **no Snapshot ID on the
  port**, because `af/ArtifactInputV1` names one and they may have measured several; each
  artifact keeps its own subject Snapshot in its envelope, and a Measurement also in its
  payload.
- **Several outputs into a `one` port** — or any list into `source`, `history` or `sources` — is
  refused.

Every refusal is an ordinary Task-file input error raised while the revision is being built:
it names the port, the reference, and both types where they differ, before any Worker is
dispatched or any Provider admitted, and it records no Task.

### A `many` port that names no Snapshot

Three rules that held one Snapshot per port learn exactly one exception, each as narrow as its
place allows:

- The Store judges the **input** ports of a revision, a plan and an invocation — and the root
  inputs receipt, which admission already holds equal to the revision's inputs — with one extra
  case: a `many` port that names no Snapshot claims none for its artifacts. Every output port
  keeps the old rule.
- The executor lets a node's `many` input span Snapshots only when the operator's contract
  declares that port `unbound`, and then the port names none. A `same_as` or `derived_from` port
  still spans one, and in the compiler a root port without a Snapshot has a lineage of its own,
  so it satisfies neither against another input.
- The Worker renderer shows every value of such a port with the Snapshot its own envelope names.

### The record and its display

`af/TaskInputBindings@1` keeps one entry per bound port, as ADR-0117 wrote it. A port bound from
several outputs keeps the first in that entry and each further one, in order, in one optional
`also` list: `{ artifact_id, snapshot_id?, task }`, never re-rooted and never nested, each naming
its Task, and every artifact it names among the record's references. `also` is absent for every
port bound from one reference, so each record ADR-0117 wrote is byte-identical and still valid.
This changes the contract in place under clause 3 of
[ADR-0113](0113-ga-reads-only-what-ga-writes.md), which allows it before GA.

`af task explain` annotates the `IN` row with every output (`measurements <- task
experiment/baseline + task experiment/candidate`), and `--tree` and `af task show` print their
existing `BOUND` group and `bound` line once per output, in the same shape. A Task without
`inputs` writes no record, so its revision, plan and inspection documents do not change.

The shipped `builtin/report` starter's generated Worker schemas admit, never require, a
`snapshot_id` on `comparison` and `measurements` values, because a Measurement or a comparison
always names the Snapshot it measured.

### Authority

Unchanged from ADR-0117: a bound input carries provenance only. No acceptance, verification,
plan approval, delivery or budget crosses a Task boundary through it, and every refusal ADR-0117
made stays.

## Consequences

An experiment's answer reaches the next Task by identity: a report reads the exact comparison
and both Measurements its predecessor recorded, and the context manifests of its author and its
verifier name those artifacts. `crates/af/tests/task_research_chain.rs` proves it with the
shipped starters in one Store.

The one-Snapshot-per-port rule now has an exception, stated in three places. It is the price of
not lying about which tree a Measurement measured. It applies to input ports only, and to a
Worker only through an `unbound` declaration.

This repository's staged and installed `kernel/analyst` and `kernel/report-verifier` packages
(`fixtures/kernel-report/`, `.af/task-packages/kernel/`) declared `comparison` and
`measurements` item schemas without a `snapshot_id` member, although a value of either port
always carries one. A Worker may not edit `.af/`, and the idempotent-install test holds the
staged packages equal to the installed ones, so this package could not make the change; a human
gave both schemas an optional `snapshot_id`, reinstalled the packages and repinned them in the
same commit as this package's review fixes.

## After the first verification

The package's review (Task `research-r4`) changed two rules, and the text above reads as amended:

- **An unnamed Pipeline's bound port must be declared by every Pipeline accepting the kind.** A
  port that some accepting Pipeline lacks is refused with a request to name the Pipeline, before
  selection could let the binding choose the Pipeline.
- **Listed outputs are judged before the list's shape.** A list bound to a `one` port first
  resolves each reference, so a name that is not a result output, or an output of another type,
  is refused for that reason rather than only for the list's cardinality.
- **A bound `many` output records every artifact it holds**, one entry each, so the binding
  record, its references and `af task explain --tree` show everything the binding delivers. The
  record is bounded at 1,024 artifacts per port; the Task file's list keeps its sixteen
  references, which is a bound on references, not on the artifacts an output holds.
- **Adapter-owned ports are checked against the selected Pipeline too.** `source`, `history` and
  `sources` keep their profile types, but a Pipeline that does not declare the port, or declares
  it differently, refuses the binding by name with both types at resolution, not at compilation.
