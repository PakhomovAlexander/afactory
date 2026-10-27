# ADR-0238: Retain refused physical telemetry measurements in incomplete results

**Status:** proposed (2026-10-27). Extends ADR-0214/0215 and preserves ADR-0222/0223 ownership.

## Context

Hosted run 40483040277, measure 122121117110, failed during finalization with
`invalid_telemetry_run: decimal: a metric decimal is nonnegative`, followed by `missing_result`.
The original field is unknown; drain latency is a hypothesis, not established incident evidence.
Some script schemas permit signed numbers in physical measurement fields. Recomputing a script's
statistics alone therefore admits negative samples, including ones hidden by a positive median,
until cross-revision comparison formatting refuses a nonnegative decimal and loses the result.

## Decision

The controller checks every schema-declared numeric field before summarization and admission.
Every numeric field must be finite, including overflow exponents. Physical wall durations must be
positive; raw drain/read latency, individual samples and byte/count measurements are nonnegative.
Values must fit the result's existing exact decimal/rational contracts. Derived differences,
percentages and intervals may be signed. No values are clamped and the workload, statistical
rules, schedule, cleanup policy, deadlines and retry policy are unchanged.

Result v2 adds required nullable `measurement_refusal` to each run. It retains a bounded sanitized
projection of the first invalid original measurement: the schema-declared field path, the exact
numeric token up to 512 bytes, complete token byte count and digest, original document byte count
and digest, and typed reason. No arbitrary candidate key or free-form document string is retained.
The refused run has `refused_document`, no admitted document or judgment, and a readable diagnostic
naming experiment, round and side. Other planned runs continue; existing `partial` and `failed`
result states suffice. Existing owner fences and work cleanup apply before result publication.

Offline verification resolves the retained field through the exact authority's product schema,
checks the token identity and replays the refusal reason, then recomputes figures and reports
from successful runs alone. A capped oversized token remains explicitly incomplete evidence;
its prefix and full identity are retained, not a claim that its unretained suffix was replayed.
Upload and history/collection retain their existing whole-bundle verification and credential
screening. Writers, readers, schema and parity fixtures move together; version 1 fails closed,
without migration or fallback.

Retained paths spell complete schema property names, including digits such as `machine.load1`,
and canonical bounded array indices. Offline resolution consumes the whole path, checks declared
array bounds and reaches exactly one numeric schema node; partial name matches, undeclared keys
and malformed paths cannot supply refusal authority. Before converting a bounded original numeric
token to a float, admission inspects its mantissa's sign and nonzero digits without expanding
exponents. A negative nonzero physical original is negative even if float conversion rounds it
to signed zero; any other nonzero original that rounds to zero is unrepresentable. Legitimate
signed zero and representable signed derived differences retain their existing contracts.

## Consequences

Malformed telemetry measurements become diagnostic incomplete results instead of aborting all
finalization. Reports and job summaries expose the precise refused field. Sanitized evidence
retains the offending value and original identities, not a replayable whole rejected document;
the original document cannot be recovered from a digest. Stand-in controller and verifier
regressions cover both sides, masked samples, nonphysical/nonfinite values, signed derived
figures, mixed/all-refused runs, verified upload/history and tampered evidence. They establish
source behavior, not the unknown incident field, real hosted performance or rollout readiness.
