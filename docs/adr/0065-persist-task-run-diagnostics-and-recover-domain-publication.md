# ADR-0065: Persist Task run diagnostics and recover domain publication

Status: accepted for the Task increment implementation; unreleased. Superseded in part by
[ADR-0113](0113-ga-reads-only-what-ga-writes.md): readability of older Tasks without run reports.
Every run report is `af/TaskRunReport@2`, the only run report; an Integration phase report is the
same contract with a `phase_id`.

## Context

A context-preparation failure can occur before a Task Attempt exists. Previously the scheduler
returned its error in memory, while inspection retained only Attempt failures and the final
domain conclusion. Review migration also needs a recovery boundary between common Task output
publication and canonical Review evidence publication. Losing the latter acknowledgement must
not start another paid Worker or make unpublished domain evidence available downstream.

## Decision

After each scheduler run, the common runtime records `af/TaskRunReport@1` through a trusted
`RunReported` Task transition. The report binds the current revision, plan, observed event
sequence and every compiled node in plan order. Completed entries reference the exact common
output receipt; failed entries reference a bounded `af/TaskDiagnostic@1`; suppressed entries
name their reason. Diagnostic truncation is explicit. Store admission rejects another revision,
plan, sequence, incomplete node order or fabricated completion. Reports are observations and
never supply acceptance evidence or authorize outputs.

The common runtime settles and publishes an output before calling the trusted host's
`commit_domain_output` hook. The scheduler publishes that node to downstream consumers only
after the hook returns successfully. The hook is idempotent and cannot start paid work or
change Task accounting. Replay calls the hook again for the same published output; domains
must verify their existing publication against that exact identity. The Store lock is released
before the hook so domain evidence can use the same serialized connection.

A failed or panicking hook retains its settled charge and output, records a domain-publication
diagnostic, and leaves the Task waiting for human recovery. The CLI does not finish that waiting
Task. An explicit run can resume this recorded publication failure under the existing lease,
plan and approval checks. Other waiting reasons retain their existing admission rules. A waiting
Task cannot claim satisfied acceptance.

Task inspection includes durable run reports and their node diagnostics. Text output shows the
latest run's failures, including failures with zero Attempts. Older Tasks remain readable with
an empty report list; frozen Review Kernel reports and legacy lifecycle events are unchanged.

## Evidence and migration boundary

Tests reopen the Store after lost publication acknowledgement, retain one Attempt and its exact
seven-token fixture charge, recover the same domain proof, and replay without another Worker
call. A separate pre-context refusal remains inspectable after reopening with zero Attempts.
Store forgery tests reject stale or incomplete reports and invented completion. JSON schema
parity checks cover closed variants, identity, safe integer and Unicode diagnostic bounds.

This supplies the common recovery boundary for P06. The existing legacy Review CLI still needs
its business adapters and selected-evidence links wired through this boundary; no legacy
Attempt events are fabricated and no migration completion is claimed here.
