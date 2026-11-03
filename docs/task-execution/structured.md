# Structured Tasks

A package-defined business kind can use the installed `structured` profile to process captured
JSON and return typed data. Completion has domain conclusion `structurally_validated`: the
kernel checked schemas, exact input identities and provenance. It does not mean independent
semantic verification, input authenticity or code delivery. The generic Task status `verified`
means this Task's declared obligations passed; inspect its domain conclusion for the guarantee.

The decision is
[ADR-2026-11-03-accept-package-defined-structured-tasks](../adr/2026-11-03-accept-package-defined-structured-tasks.md).

## Package contract

A kind package uses `schema = "af.task-kind/2"`, `profile = "structured"`, and its business
`kind`, plus `structured.json` with `schema: af.structured-contract/1`, `inputs`, `outputs`, and
`require_container`. Every port has `artifact_type` and an embedded local `schema`. Only
`custom/Name@N` artifact types are admitted; kernel port names and artifact types are reserved.
There are one to four required inputs and one to four required outputs, each cardinality one,
without a source Snapshot. Payloads and files are at most 64 KiB each; the contract is at most
256 KiB. Schemas cannot resolve remote files or URLs. Task input bytes and schema/package
changes change the captured plan.

The Pipeline declares `af.pipeline/4`, one Worker slot and `structured_accept`. It wires every
root input to both nodes and each Worker output to `result_<port>` on acceptance. Acceptance
returns each business port unchanged and `validation: af/StructuredValidation@1`, covering
`structured`. The Worker's output schemas equal the kind's; every output retains every root
input. There is no package check executable in this profile. Effects are empty. A container can
be required by the captured contract; `false` uses the existing trusted-local data environment.
That choice is reviewed authority, never an instruction in captured input.

## Capture, run and inspect

After installing and committing reviewed package pins and a business-kind mapping:

```sh
# Files are relative to the Task file, not the current directory.
af task plan --file examples/task.json --input-json issue=issue.json --json
# Review the exact plan ID printed above before confirming it.
af task run triage-one --confirm-plan sha256:... --json
af task output triage-one --port triage --format json
```

`start` accepts the same repeated `--input-json PORT=FILE` flags. Inputs are regular local JSON
files, without symlink leaves or path components or `..` escapes. An unknown, duplicate,
missing, oversized, malformed or schema-invalid input refuses before Provider admission.
A recorded Task output can instead bind a declared input with the existing Task file `inputs`
syntax. File and recorded bindings cannot claim the same port. Raw file bytes and schema
identity are retained in CAS. Retry and resume never reread the file or mutable package. An existing structured Task must
use `af task run` to reuse its exact capture, or explicit `plan --replace` to recapture while
unstarted. Recapture also captures current package schemas and invalidates the earlier plan
approval; once execution has begun it requires a new Task ID.

Repository rules, source files and read-first context are not implicit structured inputs.
The standard model adapter delivers only bounded declared inputs, instructions, output schemas
and admitted retry feedback. The Worker supplies payloads, with `source_artifact_ids` containing
the sorted exact IDs of all custom input artifacts. The kernel constructs canonical envelopes
and validation evidence. A Worker cannot claim a receipt by adding a JSON field or output port.
Malformed replies receive normal durable feedback within the existing Attempt limits. Failures
and cancellation never imply acceptance. Resuming a finished Task reads its recorded result
without another Worker Attempt. `af task output --format json` writes the canonical envelope.
`af task deliver` refuses structured data Tasks before any Git mutation.

## Consumers

The [staged examples](../../fixtures/custom-tasks/README.md) contain a single-issue triage Task,
a non-triage capitalization kind, and a native Scenario. Scenario output projection selects the
exact business output artifact ID of the recorded Task result, under the pinned Task plan and
revision. It does not create a new Task, certify the business claim or authorize publication.
