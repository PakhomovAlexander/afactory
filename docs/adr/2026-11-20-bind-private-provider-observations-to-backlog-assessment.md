# ADR-2026-11-20-bind-private-provider-observations-to-backlog-assessment: Bind private provider observations to backlog assessment

**Status:** proposed (2026-11-20).

## Context

The [daily backlog assessment](2026-11-20-prepare-private-daily-backlog-assessments.md)
uses the receiving Project's frozen batch and tracked items. An external ticket can already cover
a freshly reported defect. The offline issue observer emits a bounded, versioned GitHub
page walk, including stable issue identities and per-ticket truncation.

## Considered options

- Query GitHub in the Task. This gives Workers provider credentials and lets the author and
  verifier see different observations.
- Use titles, item markers or historical imports. These do not demonstrate defect equivalence
  and can incorrectly suppress work or copy private text into records for publication.
- Pin a read-only observation under the receiving Project's private authority and use the
  same captured input in the author and independent verifier.

## Decision

The optional Project-owned index pin binds exact observation bytes, the active batch, the
committed source revision and the destination. Collection checks the private file and the
GitHub repository identity against the batch's pinned destination configuration. The Project
rejects incomplete or over-bound observations and captures one bounded complete walk in the Task
input. The independent verifier uses that exact observation and the committed source to judge
whether a cited body line describes the same behavior and location. The kernel checks the stable
issue ID, the quoted body line and per-member source evidence before accepting the disposition.
Truncated text cannot support an exact match. A truncated body blocks proposing public fields
from that observation. Plausible but inconclusive duplicates remain private.

Before first settlement, the Project checks the collection against its active authority.
The private decision records the observation identity and, for an accepted match, the verified
GitHub issue ID and number alongside the Task receipts. Settlement replay uses its accepted
receipt. The observer remains read-only, and the verified public fields pass through the existing
publication authority and budgets.

## Consequences

Both Workers can assess the same provider evidence without a live lookup. A complete live walk
still does not prove absence, and the accepted observation may become stale. Receivers with no
index pin retain local item matching and hold suspected duplicates with unknown identity. Provider
tickets remain observations rather than Project Reports or items.
