# ADR-0095 — Bind legacy Task context and retry output admission

**Status:** accepted for the unreleased Task increment, 2026-09-13. Superseded in part by
[ADR-0113](0113-ga-reads-only-what-ga-writes.md): the whole *Explicit legacy context* section
and the first two paragraphs of *Alternatives and consequences*, since the `legacy_task_command`
runner, `af/TaskContext@2`, the compatibility contracts and the separate metadata reader are
gone; the output-admission retry decision and its rationale stand.

## Decision

This extends [ADR-0049](0049-run-task-workers-through-shared-durable-attempts.md) and
ADR-0051 for new captures.
Their accepted text and previously persisted artifacts remain unchanged.

Schema-valid Worker payloads can still fail domain admission. The Store records that outcome
as a failed Attempt with the actual reported usage and typed `output_admission_rejected`
feedback, then the common runtime may use the declared retry allowance. Diagnostic prose is
not retry input. A CAS, authority or persistence error remains an operational failure and
does not authorize another Worker dispatch. Feedback persistence failure leaves the recorded
Attempt and known usage recoverable rather than hiding a charge or claiming successful output.

## Explicit legacy context

New `legacy_task_command` packages declare `runner.legacy_budget_tokens`, including explicit
zero. It is a nonnegative safe integer and describes the original wire budget; command
Attempt reservations remain zero model tokens. The fixed adapter captures the pinned
`pipeline.attempt_tokens` value in this field. Generic Task files use the same runner without
adding Task identity or budget metadata to the business Requirements artifact.

New compatibility contracts bind this value and publish `af/TaskContext@2` with the captured
Task revision, Task identity and ExecutionPlan identity. Preparation, dispatch and replay
validate those bindings. An absent or changed binding is a refusal, not an ambient default.
Previously captured packages without this field retain their original compatibility contract,
`af/TaskContext@1` serialization and Requirements-based wire reader. New capture refuses the
old missing-field shape; persisted artifacts are never rewritten to manufacture new authority.

The new context generation reads current/source Manifest metadata through a separate 8 MiB
compatibility limit. This is a new host metadata limit, not a general Manifest contract. It
validates canonical path order/encoding, entry fields, content identities and each Snapshot's
Manifest digest, without retrieving every file blob to derive mutation paths. Materialization
and output admission still verify file content at their own boundaries. Internal lookup
identities are recorded; only rendered/retrieved Worker context contributes context bytes and
tokens. The final Worker input remains bounded to 1 MiB. Old captured contexts keep their
original rendering and bounds so that their identities and retry inputs remain exact.

## Alternatives and consequences

Injecting execution identity or budget into business Requirements would make generic Task
files depend on a hidden fixed-adapter input shape. Inferring the legacy wire budget from
command reservations would silently turn an existing nonzero wire value into zero. Explicit
captured context keeps those authorities separate and permits exact old-context reads.

Raising the delivered Worker limit would expose unrelated repository metadata and increase
context charges. Rehashing all file blobs merely to derive mutation paths would repeat work
owned by materialization and admission. The separate finite metadata reader preserves those
boundaries while still refusing malformed or oversized metadata and oversized rendered input.

Treating every settlement error as retryable would conflate output rejection with failed
persistence. The Store returns a typed domain-rejection outcome only after recording the
failed Attempt; operational Store errors retain their existing failure path.
