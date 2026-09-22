# ADR-0085: Retain exact native Task usage across multiple turns

Date: 2026-09-12
Status: Proposed; superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): the
frozen Kernel's separate `AttemptEvidence`. The exact Task Attempt evidence now carries that name.

## Context

The common Task ledger already retains cumulative u128 charges, but native adapters can lose
usage before it reaches that ledger. Adding individually valid u64 input and output counts can
overflow u64. Summing multiple native turns can also overflow an individual component. Clamping
those values makes later failure recovery and budget enforcement operate on understated usage.
The existing Task usage generation allows wide charge but still bounds each component to u64.

## Decision

Normalize Task-only native returns, work outputs, wall sidecars and selected Attempt evidence
to exact u128 components and charge. Introduce `TaskTokenUsage@3` with decimal-string components
and exact totals. Claude Task usage adds its reported components without saturation. Codex
Task usage folds each native turn in a separate parser. Its u64 native counters and bounded
input stream keep their aggregate within u128; totals are neither clamped nor narrowed. Preserve the
frozen legacy Reviewer adapter and its original parser rather than changing historical meaning.

Retain observed usage independently of business decoding, file reads or CAS publication
failures. Native capture keeps stdout available even when its raw CAS write fails, so adapters
can still recover usage. Timeout, failed output, unavailable CAS and later lease recovery retain
that exact observed floor. Common
accounting keeps the original reservation and paid history; an overrun cannot renew resources
or authorize another call. This change does not alter which native counters are chargeable.

Preserve the existing encoding when its values remain representable. Generic Task capture uses
usage@2 for narrow components and usage@3 only for wider components. Captured Review preserves
its original usage/provenance identities when all their original fields fit. Otherwise
`TaskReviewAttemptProvenance@2` carries exact charge and the appropriate exact usage artifact.
The frozen Kernel's `AttemptEvidence` remains separate from Task-only exact Attempt evidence.

Add a v3 sidecar column without deleting original v1/v2 bytes. Read the newest non-NULL SQL
column; malformed JSON, including JSON null, refuses rather than falling back to older bytes.
When recording a new observation, merge the cumulative floor with existing usage. An absent
new observation cannot erase earlier known usage. Old rows widen losslessly. Any requested
legacy downcast checks every field and reports overflow instead of dropping or clamping data.

Selected Review output uses `af/review-outcome@3` and `af/review-report@4` when individual values
require the wider contract. Representable output keeps its existing generation. Whole-Task
cumulative accounting remains distinct from selected reviewer usage. Existing execution records,
original Task limits and the common budget need no new execution authority or allowance.

## Considered options

- Saturating sums or omitted usage lose paid work before the ledger can protect it.
- Widening only charge still loses multi-turn input, cache, output or reasoning components.
- Reinterpreting old schemas would break strict readers and content identities.
- An additive exact type with checked narrow encoding preserves both old bytes and new facts.

## Consequences

Native overflow remains visible through successful output, failed output, timeout, CAS outage,
sidecar recovery and reopened inspection. A large observation may exhaust the original Task;
it does not make completed work free or extend execution. Focused tests include actual synthetic
native executables and preservation of frozen legacy fixtures. Full checkpoint verification,
specialist review and live Provider evidence remain separately recorded work.
