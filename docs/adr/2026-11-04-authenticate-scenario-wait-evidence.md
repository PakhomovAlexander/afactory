# ADR-2026-11-04-authenticate-scenario-wait-evidence: Authenticate Scenario Wait evidence

**Status:** proposed (2026-11-04).

## Context

A pinned research index proves bytes, but cannot establish native provenance. A definitive
source-capture refusal must also settle the common fence without hiding uncertain cleanup.
These repairs enforce the native boundaries of
[the Scenario integration decision](2026-11-03-native-performance-scenario-loops.md).

## Considered options

- Trust owner-pinned index JSON: rejected because fabricated native receipts remain possible.
- Clear every failed capture's fence: rejected because torn captures and surviving processes
  require operator inspection.
- Add a parallel evidence provenance format: rejected because native readers and offline
  original verification already supply the authoritative facts.

## Decision

The bounded evidence Wait rederives its selected records through the native Task evidence
reader and journal originals. Completed originals require offline verification under the common
exclusion. Every selected bounded fact must match, including content and receipt identities,
summary and outcome. Owner-selected compatibility and observation time retain their declared
meaning. Omitted records do not enter research. Existing byte and item bounds remain binding;
no persisted shape changes.

Source capture reports an explicit clean refusal only before capture publication and after a
normal failed subprocess exit with no remaining process group, or a pure preflight refusal.
Journal exclusion settles only that confirmed refusal. Timeouts, torn captures, abnormal exits
and surviving descendants remain uncertain and fenced. Submission records that uncertainty
rather than declaring it a definitive refusal solely because no experiment request exists.

## Consequences

Research observations require native sources to remain accessible. Changed or fabricated index
facts cannot satisfy a Wait even when the owner pins their bytes. Definitive capture refusals
no longer starve unrelated submissions; ambiguous cleanup still blocks the common machine.
