# Report artifacts are authoritative for finding projections

**Status:** accepted (2026-08-20)

`FindingReported@1` references an immutable Report artifact but also duplicates part of the
Report in its payload. Adding the missing `fix` field to that payload would create another
authoritative-looking copy and would change an `@1` event shape despite ADR-0002. We instead
treat the referenced Report as the authority for claim content: Ledger replay projects body,
fix, confidence, severity, and location from it, while the event supplies admission and
transition metadata. Existing duplicated `@1` fields are frozen as a compatibility fallback
only for imported legacy rows that have no Report artifact.

## Consequences

- Rebuilding a Ledger requires the event stream and every artifact it references. A missing or
  corrupt referenced Report makes the run invalid rather than producing an incomplete Finding.
- Artifact-less legacy imports may have no fix. The projection represents that absence and the
  CLI labels it unavailable; it never fabricates a remedy.
- Future Report fields become visible through the Report contract rather than by expanding event
  payloads. A genuine event-payload change still follows ADR-0002 and receives a new event type
  version.
