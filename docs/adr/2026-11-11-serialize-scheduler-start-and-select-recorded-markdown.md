# ADR-2026-11-11-serialize-scheduler-start-and-select-recorded-markdown: Serialize scheduler startup and select recorded Markdown

**Status:** proposed. **Date:** 2026-11-11.
Extends ADR-0274 and the proposed native-intervals-and-recorded-result-markdown decision.

## Context

Scheduler startup acquired the exclusive driver lease before replacing its status row. A concurrent
stop could be acknowledged in that window and then erased by initialization. The original result
renderer also required JSON even when a named text result was already authored as Markdown.

## Considered options

- Reset the stop flag before acquiring the lease: rejected because a second driver could erase the
  running driver's acknowledged request.
- Persist every stop forever: rejected because a later explicit restart must remain possible.
- Guess Markdown when JSON parsing fails: rejected because unsupported structured shapes must fail.
- Use the render or Run-modification clock as observation time: rejected because neither establishes
  when the source was observed.
- Require another JSON wrapper around existing Markdown: rejected because native text already owns
  its content and provenance.

## Decision

Begin an immediate Store transaction before acquiring the exclusive scheduler lease. Initialize the
scheduler row under that same transaction and commit before entering the driver loop. Stop updates
serialize with initialization, on both first startup and restart. A stop acknowledged after the
lease is acquired cannot be reset by that generation. A later explicitly invoked generation clears
the previous generation's stop request. Lease-based crash/liveness semantics, graceful bounded
current dispatch and heartbeat-before-tick behavior remain unchanged. A private unit-test seam
controls the startup window; there is no production test flag.

Expose `--raw-markdown` on native result observation, requiring a named text output, a distinct
`--observed-output` recorded integer port and a positive `--stale-after-ms` freshness bound. These
arguments declare a read-only projection, not new persisted producer content. Both metadata and
content are fetched in the same indexed native read. Missing or unsupported metadata fails visibly;
no observation time is synthesized. The renderer projects the exact text into the existing validated
Markdown block and snapshot contract. Existing named-output, Run/occurrence provenance, revision,
partial/final/empty/error and stale/fresh semantics, size and terminal-control checks apply. Default
structured JSON presentation and structured-cell escaping are unchanged.

## Consequences

No persisted typed shape changes, schema generations, migrations, services, credentials or
publication authority are introduced by these two repairs. Structured and raw presentations use
current `af/result-document@1` and `af/result-snapshot@1`; complete underlying producer text and
metadata remain available through native inspection. Watch only reads and atomically renders.

These repairs do not resolve the separately recorded sustained-retention defect. The unchanged
16 MiB World journal can still prevent lifecycle transitions when full. No receipt or accounting
is discarded, no bound is raised and no shadow ledger is introduced to conceal that limitation.
Requirements acceptance remains blocked until bounded native retention with protected lifecycle
capacity and its sustained-use regression is implemented and independently verified.
