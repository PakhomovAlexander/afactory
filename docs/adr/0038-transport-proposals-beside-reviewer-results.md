# Transport proposal declarations beside Reviewer Results

**Status:** accepted (2026-08-31); superseded in part by
[ADR-0113](0113-ga-reads-only-what-ga-writes.md): `ReviewerResult@1`. The runner extracts the
declaration before normalizing `ReviewerResult@2`, the only reviewer result.

Reviewers may return one optional Proposal declaration beside the flat fields of their final
Reviewer Result. The declaration carries the exact Git patch text, its complete declared path
set, same-result Report indexes and/or prior Finding IDs, Evidence IDs, description, and an
auto-apply nomination. The runner extracts this transport field before normalizing the established
`ReviewerResult@1` or `ReviewerResult@2`; the persisted Reviewer Result therefore retains exactly
the wire and artifact contract fixed by ADR-0021.

The kernel seals the Attempt sandbox and independently derives its complete canonical Git diff.
It refuses a declaration whose patch bytes or path set differ, whose claims do not resolve, or
whose shape is invalid. A valid declaration is durably prepared with the selected Attempt before
the node receipt. At the canonical Ledger barrier, same-result Report indexes are replaced with
the typed Report IDs produced by reduction and one `PatchProposal@1` artifact is published. A
prepared declaration cannot become a Proposal if its Reviewer Result is not selected or routed
through the authoritative Ledger.

## Considered options

- **Put Proposal fields into `ReviewerResult@1`.** Rejected because ADR-0021 freezes that artifact
  as the flat finding wire; proposal lifecycle and result admission have different authorities.
- **Derive every sandbox mutation set as an implicit Proposal.** Rejected because build output,
  diagnostics, and an unreverted probe could then ride along as operator-facing code without the
  reviewer naming the exact intended patch.
- **Ask for one patch per Finding.** Rejected by ADR-0010: one shared sandbox mutation set was
  verified atomically and cannot truthfully be split into independently selectable fixes.
- **Keep a valid declaration only in process memory until reduction.** Rejected because a crash
  after reviewer admission would make replay lose a paid, selected output.
- **Extract one transport declaration, verify it at seal, and finalize it at the Ledger barrier
  (chosen).** This keeps established results stable, makes mismatch refusal executable, and binds
  final claim IDs at the first point where they exist.

## Consequences

- A reviewer may emit no Proposal or exactly one; arrays of candidate patches are not admitted.
- Patch text is repeated in the model response and charged as output. Pipeline budgets are raised
  deliberately for proposal-capable reviewers rather than hiding that cost.
- Prepared proposal state is durable but has no operator-facing Proposal ID. Only the finalized
  `PatchProposal@1` envelope is exportable or eligible for later Integration.
- Proposal events and artifacts remain outside graph result ports. Existing reviewer receipts
  still name one selected Reviewer Result, while semantic closure separately proves the finalized
  Proposal reaches its sink.
