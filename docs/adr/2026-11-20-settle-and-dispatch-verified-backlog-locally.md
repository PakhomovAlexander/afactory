# ADR-2026-11-20-settle-and-dispatch-verified-backlog-locally: Settle and dispatch verified backlog locally

**Status:** proposed (2026-11-20).

## Context

The [frozen batch journal](2026-11-20-freeze-project-backlog-at-authority-cutoff.md)
names the Reports a daily occurrence may process. The
[portable assessment](2026-11-20-prepare-private-daily-backlog-assessments.md)
produces an independent Report verification receipt. A completed Task alone grants
no Project write or delivery authority.

## Considered options

- Have the Worker record decisions or contact a destination. This would grant a
  model authority beyond its private Task and couple verification to delivery.
- Have the operator transcribe results into batch commands. This would lose the
  binding to the assessed fields and independent receipt on resume.
- Let native Scenario Actions settle and deliver under the receiving Project's
  pinned local policy.

## Decision

Preparation resolves a frozen Project batch from local authority, captures the
committed portable artifact and pins a read-only Report Task and two builtin Actions.
The operator confirms the prepared Scenario pin. Its local Project policy binds
the artifact digest, authority, limits and synthetic destination.

Settlement checks the exact accepted native result, independent verification,
Document, draft, source Snapshot and captured batch against Project authority.
It records typed grouped decisions and item revisions in one Project Store
transaction. Each positive group requires a policy-permitted evidence digest for
every member that matches a cited, bounded file in the committed Snapshot. The
Workers compare the private facts and bounded excerpts with the Snapshot to verify the reported
behavior and assess privacy. A local duplicate retains the matched item and revision from the
frozen view. A hold or unavailable evidence
prevents public fields from being retained in the visible view.

Dispatch requires the matching settlement Action receipt. The native host checks
the current revision and installed policy and reserves at most the publication
count limit. Verification makes approved items visible even when capacity
defers delivery. The close transaction records pending revisions and decisions. A later occurrence
can dispatch them without new intake after rechecking eligibility. A superseding revision or
a changed source marks a pending delivery as requiring reverification. The Project Store records an intent and attempt before
inserting the approved revision into an owner-only local sink. The sink key
binds the destination and revision. Reconciliation accepts existing exact
bytes for that key and records the receipt without a second delivery. The daily
journal closes after all member dispositions and admitted writes are resolved.

## Consequences

Project readers can trace each Report through its decision and the retained
revision history. Future adapters can consume that same settlement, subject to
their own destination authority and reconciliation rules. This decision covers
only the synthetic local sink and requires no destination credentials.
