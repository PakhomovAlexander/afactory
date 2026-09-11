# ADR-0061: Capture read-only issue sources outside execution authority

**Status:** accepted

## Context

A ticket must become exact Task input before selection. Treating a Jira timestamp as an immutable
version, fetching again during resume or silently dropping unsupported rich-text requirements
would let the request change underneath a plan. Source content must never configure Workers,
permissions, verification or resource bounds.

## Decision

Use the `review-source-task` boundary for read-only issue adapters. It returns bounded normalized
requirements and captured provenance and has no scheduler dependency. Local JSON/TOML and Jira
Cloud adapters produce the same `IssueInputV1` contract. Task-file `issue` selects the adapter;
its optional structured `requirements` remains explicit business data. The original action plus
normalized issue text becomes the Task goal, so the Planner sees the actual business request.
The declared Requirements input contains the normalized text and optional specification.

Capture exact raw source bytes, source identity/revision labels and each selected field's canonical
value and normalized-text digests. Jira's `updated` value is a source label, not an immutable
version guarantee. The raw response supplies exact observation identity. Preserve original ADF
values alongside bounded ordered text. Unsupported nodes, marks, absent selected fields and
oversized inputs fail explicitly. Links are captured data and never trigger retrieval.

Native Jira capture owns one HTTPS GET to a validated Cloud tenant and issue key. Required fields
are summary, description and updated; explicitly selected custom acceptance fields are bounded,
sorted and unique. The local binding supplies the tenant and credentials. Credentials enter the
owned curl process through stdin, never argv, captured policy or Worker input. Disable curl's
ambient configuration, proxies, redirects and URL globbing; keep certificate verification. Bound
time, body bytes and process lifetime, and return typed errors without response-body diagnostics.
There is no automatic source retry. The shared process supervisor owns cancellation and termination.

Only initial explicit capture reads the source. A recorded Task run or replay uses its captured
Requirements and source records. Local issue files come from the selected Git Snapshot. Jira raw
responses and undeclared fields stay outside the Worker payload. Source bindings do not appear in
exported definitions or Task authority.

## Consequences

Conformance fixtures compare JSON, TOML and recorded Jira normalization, per-field identity,
changed fields under an unchanged timestamp, unsupported content, failures, cancellation and byte
limits. A real CLI fixture captures local issue input, executes shared implementation and embedded
Review, replays after source-file edits and explicitly delivers a verified new worktree.
Command tutorial Workers assess their declared structured specification; broader interpretation
requires suitable replacement Workers. No live Jira account or model-performance claim follows
from recorded fixtures. Explicit source refresh and its revision/accounting barrier remain the
next P13 checkpoint; resuming never implicitly refreshes a ticket.
