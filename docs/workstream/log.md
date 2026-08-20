# Review Kernel workstream log

## 2026-08-20 — M0 and M1

Closed the append-only event vocabulary, structural run reports, typed invocation/output ports,
and canonical report projection. Added complete operator rendering through `show`, long ledger,
and markdown report commands.

## 2026-08-20 — M2.1 capability-negotiation slice

Added versioned pipeline Subject configuration and digest-pinned reviewer capability declarations.
Legacy pipeline and package formats remain whole-tree-only. Unsupported diff execution, inline
diff reviewers, empty reviewer sets, unsafe registry names, symlink/non-regular package entries,
and lossy package paths fail closed. Shipped reviewers remain whole-tree-only until M2.2–M2.4
provide pinned authority, Base, Subject, and Change Set artifacts. Runtime `Subject@1` publication
remains the first M2.2 integration slice; this entry does not claim it landed.

## 2026-08-20 — M2.2 immutable authority bootstrap

Added `CampaignManifest@1`, `CampaignOpened@1`, `Subject@1`, `RoundStarted@1`, and explicit
`RoundInputSuperseded@1` epochs. A new Campaign captures its trusted Authority Snapshot before
candidate capture and publishes exact pipeline, lock, package, policy, convergence, budget,
focus, and genesis-root identities. Continuation reconstructs reviewer packages from captured CAS
bytes and never re-reads live package paths. Incomplete Rounds reuse exact Subject and input-set
IDs; capturing a changed head requires `--restart-round`. Node invocations, output receipts,
attempt lifecycle events, and run conclusions are causally and artifact-bound to their Round,
Campaign Manifest, Authority Snapshot, and Subject.

The next slice is M2.3's typed, configuration-neutral Git tree diff, followed by M2.4 Change Set
publication and port wiring. Diff Campaigns pin Authority/Base now but still fail before dispatch.
