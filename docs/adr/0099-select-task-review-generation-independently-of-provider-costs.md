# ADR-0099: Select Task Review generation independently of Provider costs

Status: accepted for the authorized Task correction, 2026-09-14. Superseded in part by
[ADR-0113](0113-ga-reads-only-what-ga-writes.md): the semantics of captured policy-one plans,
and omission selecting policy one. An omitted `review.generation` now selects generation two,
the only generation.

This supersedes only the catalog-selection paragraph of
[ADR-0094](0094-bind-task-review-assignments-and-readable-inputs.md). Its assignment, provenance,
readable-input and generation-one replay decisions remain unchanged.

PR2 already assigns `af.task-catalog/2` to explicit Provider admission costs. Selecting Review
contracts from that catalog version would reinterpret independently captured authority. Review
instead declares `review.generation = 2`. Omission selects the existing
`af.review-task-policy/1`; explicit values other than two are refused. The new selector produces
`af.review-task-policy/2` with matching Subject, assignment and ReviewerResult contracts.
Catalog and Provider-admission generation rules remain unchanged.

New software starters and the current generic Review fixture explicitly select generation two.
Old committed fixtures remain unchanged. A newly configured heavy fixture upgrades its copied
contracts and dispositions before admission; it does not rewrite persisted execution evidence.
Restoration uses the exact captured policy artifact and package identities, never a new default
or recaptured catalog. Already captured policy-one and policy-two plans retain their semantics.

Focused controls check omitted serialization, supported and invalid explicit selectors, matching
new starter contracts, actual common execution and fresh-process replay. PR2 additionally checks
that catalog two without a Review selector still means Review policy one and that selecting
Review two preserves the original explicit Provider allowance.
