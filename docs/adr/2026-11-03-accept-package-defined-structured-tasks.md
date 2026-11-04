# ADR-2026-11-03-accept-package-defined-structured-tasks: Accept package-defined structured Tasks

Status: accepted, 2026-11-03.

## Context

One captured issue should reach one Astra medium Worker and return typed classification data.
A report Task would require a Document and independent semantic verification, and an
implementation Task would incorrectly imply code delivery. Other business kinds need the same
bounded data processing capability. Package schemas describe data; they cannot install kernel
receipts, effects or source trust.

## Considered options

- A triage profile with kernel classification vocabulary: rejected because business rules belong
  in the consuming package.
- A report alias or optional report verifier: rejected because that weakens existing acceptance.
- An external executor: rejected because it duplicates accounting, retries and Provider admission.
- An installed structural profile using the common Task runtime: chosen.

## Decision

A `structured` profile is available only through an `af.task-kind/2` package containing
`structured.json` (`af.structured-contract/1`). Existing profiles continue to use
`af.task-kind/1`; that shape refuses `structured`, and generation 2 refuses other profiles.
This is a new shape, without migration or fallback for earlier records.

The contract declares one to four required, single, unbound input ports and one to four output
ports with local captured JSON Schemas. Business artifacts use the `custom/` namespace.
Kernel names and artifact types cannot be declared. Each payload is bounded to 64 KiB and the
complete contract to 256 KiB. Schema validation uses the existing local-only Worker validator;
remote resolution and unbounded schema expansion are refused.

`af task plan` and `start` accept repeated `--input-json PORT=FILE`. Files are relative to the
Task file's directory. Capture walks descriptor-relative with no symlink following, refuses
escapes, missing and nonregular files, and checks the byte bound before parsing. The resulting
artifact retains exact raw bytes and contract identity. Existing recorded-output bindings may
supply the same declared ports, subject to the same type, cardinality and payload checks.
Every root input is resolved before Provider admission. Resumes read captured CAS data only.
Explicit `plan --replace` can recapture an unstarted structured Task even when its Task file
is unchanged, through the existing fenced revision/plan barrier. Every approval is invalidated;
execution, planning, admission or delivery state forbids recapture. A repeated plan without
`--replace` refuses rather than presenting an old capture as a fresh read.

A structured Pipeline uses `af.pipeline/4`: one ordinary Worker and one installed
`structured_accept`, plus the common root and Provider admission nodes. The compiled shape is
`af.compiled-task/3`; earlier Pipeline and graph shapes cannot carry this operator. The Worker
has no filesystem, shell, network or source effects. It executes through the standard command
or model adapter and ordinary Attempt accounting and retry feedback. The installed data-only
environment seals no filesystem mutation. There is no package executable check in this scope.

The kind's output schemas must equal the Worker's captured output schemas. Each payload also
names the sorted exact `source_artifact_ids` of all custom root inputs. The Worker retains all
root inputs in its output envelopes. The installed acceptance recomputes type, schema, source
IDs and provenance and publishes `af/StructuredValidation@1` (`af.structured-validation/1`).
That receipt binds the invocation, plan, Task revision, contract, exact inputs and outputs.
Worker JSON can never supply it. Acceptance and replay rederive it, including its kernel producer.
Only the receipt covers the `structured` obligation.

Satisfied acceptance has domain conclusion `structurally_validated`. It proves the declared
structural contract, without independent semantic verification. Existing generic status
`verified` means the Task's own obligations are satisfied; the domain conclusion and receipt
state which guarantee that means. No source Snapshot is produced, and delivery refuses before
any Git operation. Document, report, code and Review acceptance remain unchanged.

The staged triage consumer uses exactly one `gpt-6-astra`, `medium`, `codex-personal` Worker,
with the existing rubric as instructions and captured issue/knowledge data as untrusted input.
Its result is a proposal that may be held or need information. Neither successful structural
validation nor a Scenario consuming the result authorizes GitHub publication. The owner narrowed
this business flow on 2026-11-03; the older two-model publication runner design remains deferred.

## Consequences

The common runtime retains retry bounds, cancellation, usage and durable replay. A malformed
reply cannot become acceptance; replay dispatches no extra Attempt. Native Scenario Task output
projection consumes the exact recorded output through the pinned plan and Task result, without
business-specific kernel code. Scenarios still pin existing Tasks; dynamic Task creation,
publication, scheduling and installation into operator configuration are outside this capability.

Contracts and Workers are installed and reviewed as authority. Their schemas cannot grant
executable effects or change another profile's obligations. Structural validity does not prove
classification correctness, citation truth, input authenticity or freshness. Those remain
visible claims in the captured input and business output.
