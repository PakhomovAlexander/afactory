# ADR-0096: Revalidate Task execution evidence on cached replay

Date: 2026-09-13
Status: Accepted

## Context

A warm Task projection rechecked active revision, plan, decision and final-result artifacts,
then skipped the already parsed event prefix. That omitted ordinary execution records and
some raw responses, usage, diagnostics and retry feedback from subsequent integrity checks.
A fresh connection rejected missing or changed evidence that a warm connection could reuse.
Task listing also projected every Task twice: once to enumerate validated labels and once
again to format the same data.

## Decision

The private Task projection retains the exact artifact-reference closure checked while
replaying its event prefix. Every cache access verifies those bytes again, including historical
execution records and superseded evidence. Parsed state can be reused; artifact integrity
cannot. Active typed-authority checks remain, and newly replayed references join the same
per-access verification set. No public artifact, event, Task identity or accounting rule changes.

`EventStore::map_tasks` visits validated projections in Task-label order and retains only each
caller's mapped result. The CLI formats listings from those projections without a second
replay. Stream identity, full replay and artifact checks remain mandatory; a corrupt Task
refuses the listing. The Store still retains at most one Task projection between operations.

Caching successful hash checks across operations would preserve the defect. Disabling replay
memoization entirely would restore integrity at the expense of repeated parsing. Retaining
all Task graphs for a listing would increase memory with the number of Tasks; mapping each
validated projection avoids that requirement.

## Verification

Focused regressions warm the actual Store, then remove and corrupt each referenced execution
record, invocation, context, raw response, usage observation, published output and failed
Attempt diagnostic/feedback. Warm and fresh connections both refuse; restoring exact bytes
restores unchanged sequence, charge, Attempt count and output selection. Both unfinished
successful work and a finished failure retain their original evidence requirements.
A multi-Task listing proves sorted validated results and refuses a corrupt later Task.
The public Task-file listing and resume checks remain unchanged. This is a replay-correctness
and duplicate-work fix, not a measured end-to-end performance claim.
