# ADR-0077: Run captured Review operations under common Task Attempts

Date: 2026-09-12
Status: Accepted; superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): the
meaning of historical name-only Reviewer outputs as `ReviewerResult@1`. A reviewer declares a typed
`ReviewerResult@2` output, and an untyped one is refused at plan time.

## Context

The approved increment requires one execution owner for Review and implementation. Captured
compilation and canonical selection guards exist; actual transport, Gate observations and final
Review acceptance must now use common Task execution. Publication interruption must not buy
another reviewer Attempt or lose a recorded failure.

## Decision

`LegacyReviewTaskHost` executes captured Generation, Gate, Reviewer, Gather, Ledger and Slicer
operations beneath `TaskRuntime`. The runtime owns reservation, context binding, start, retry,
usage settlement and output selection. Shared Review operations preserve original artifact
codecs, Findings, Demands, Proposal disposition and canonical receipts. The host adds no Attempt
ledger or execution loop.

The same exact slot-bound model adapter performs Provider admission and downstream Review.
Captured account, backend, model, effort, invocation policy and credential mode must match before
execution. Command transport shares process capture and the adapter-owned environment.
Materialization and every Check consume the same absolute Attempt deadline. Known usage remains
outside parsing, sandbox sealing and CAS publication. Typed retry feedback names the failed
Attempt and captured contract without copying its response into the next prompt.

Every canonical Ledger exposes typed Finding Set and Demand Set companion ports even when the
historical Pipeline exported only one. Reviewer metadata and Gate outcomes also participate in
public acceptance coverage. Historical name-only Reviewer outputs mean exactly ReviewerResult@1;
this codec does not admit a different typed result.

Task selection precedes checked canonical result/Proposal publication. Reopen restores admitted
outputs and canonical receipts, so lost acknowledgements recover without another Attempt.
`TaskReviewGateFacts@1` retains bounded, non-secret cache failures in common settlement
observations, including failed Gate Attempts. Successful cache and execution-binding receipts
retain their existing canonical events.

The result assembler reads the actual durable scheduler report and selected outputs. Its
canonical RunReport references that exact Task report. Missing mandatory execution evidence
has incomplete precedence for this Task path; complete execution still requires the canonical
Review verdict to pass. Recovery after canonical conclusion but before Task finish reads the
same conclusion without dispatch into a closed Round. Historical report writers and persisted
contract versions keep their previous behavior.

Canonical conclusion publication compares the current Task writer, plan and scheduler report
and both log prefixes under one SQLite transaction. An expired or replaced writer cannot close
the Round. A report with a recoverable domain-publication failure must resume publication before
it can become a final Task result. Recording an exhausted execution's conclusion does not start
another Attempt or extend its deadline.

## Considered options

- Wrapping the original Kernel runner retains a second Attempt owner and is excluded by the
  approved design. Synthetic legacy Attempt events would also duplicate execution authority.
- Keeping cache failures only in memory changes a resumed conclusion. Typed observations
  survive the same common settlement as the failure.
- Treating successful execution as acceptance erases blocking Findings and Demands. The result
  retains their domain verdict independently of execution completion.

## Consequences and verification

This host is an internal migration checkpoint. CLI ownership, broker effects, owned Scatter
children, same-Task heavy continuation and full-width canonical report accounting still need
their remaining adapters before legacy command cutover. Values outside the frozen report's
safe numeric range are explicitly refused while the common Task ledger keeps exact usage.

Integration fixtures cover command and packaged-model execution, charged admission and failure,
binding substitution refusal, bounded malformed-response retry, Gate pass/failure, cache success
and setup failure across reopen, publication interruption on both sides of commit, blocking
Findings/required Demands, and canonical-conclusion/Task-finish interruption. Store tests constrain
the opaque V1 exception; Rust/schema parity covers the closed Gate facts.
