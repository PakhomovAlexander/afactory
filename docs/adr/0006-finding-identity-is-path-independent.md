# Finding identity is independent of path and title

**Status:** accepted (2026-08-20)

The legacy `file + normalized title` fingerprint is retained only for replaying existing
campaigns. New campaigns derive a stable Finding ID from the first selected Report ID; later
Reports attach only through an explicit corroboration relation or an exact occurrence key whose
semantics reducer policy trusts. Paths, titles, traces, and fuzzy fingerprints may suggest a
Grouping but never prove identity. Renames therefore change Report locations and Scope without
re-keying Findings.

## Considered options

- **Keep the fingerprint and emit rename re-key events.** Rejected because it makes presentation
  data authoritative, does not handle title rewording, and destructively combines independent
  claims that happen to normalize alike.
- **Silently auto-merge fuzzy matches.** Rejected because arrival order would decide identity and
  verification of one reviewer's claim could incorrectly resolve another's.
- **Path-independent IDs plus explicit relations (chosen).** This matches `FindingReport@1`,
  preserves every claim, and leaves ambiguous equivalence to recorded policy or adjudication.

## Consequences

- Existing campaigns remain readable under their recorded legacy identity policy; their keys are
  never rewritten in place.
- Grouping is reversible through compensating events and retains all Finding IDs as aliases,
  histories, and independent Report verification obligations.
- Reviewer inputs and output contracts must expose canonical Finding IDs so re-reports can state
  corroboration explicitly.
