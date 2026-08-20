# Proposals are exported by ID and remain bound to their base Snapshot

**Status:** accepted (2026-08-20); supersedes
[ADR-0004](0004-reviewers-author-verified-patch-proposals.md)

Each selected attempt may emit at most one atomic Proposal whose patch equals its complete sealed
sandbox diff and references one or more Report/Finding claims. Proposals are exported by Proposal
ID, not Finding ID: one Proposal may cover several Findings and one Finding may accumulate
competing Proposals. Verification remains permanently bound to the exact base Snapshot; a later
head makes the Proposal stale rather than re-verified or invalidating its historical evidence.

## Considered options

- **One Proposal per Finding.** Rejected because one sandbox has one final mutation set; splitting
  it after sealing would pretend its hunks were independently verified when they were tested only
  together.
- **Export by Finding key.** Rejected because multiple reviewers may propose different remedies
  and no deterministic policy makes one the operator's choice.
- **Automatically re-verify against each later head.** Rejected because applying or testing the
  patch on another Snapshot creates a new verification result, not an update to the old one.
- **Proposal-ID export with explicit stale override (chosen).** It is unambiguous, preserves
  provenance, and leaves three-way application to an informed operator.

## Consequences

- `reviewctl show` lists linked Proposal IDs, base Snapshot IDs, and current/stale applicability.
- `reviewctl export <proposal-id>` refuses stale output unless `--allow-stale` is supplied.
- The v1 kernel still does not mutate a repository. Automatic integration into internal derived
  Snapshots remains a later milestone rather than being ruled out by this export boundary.
