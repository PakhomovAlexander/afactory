# Promote only checked derived Snapshots

**Status:** accepted (2026-09-01)

Pipeline format v5 may opt into automatic Integration. Eligibility remains narrower than Proposal
acceptance: the selected Attempt's captured Execution Binding must grant `auto_apply`, its Proposal
must nominate automatic application, every claim and Evidence link must still resolve, and no
changed path may match captured protected-path policy. The kernel deduplicates identical patch
artifacts, orders candidates by captured reviewer priority and node identity, and composes only
disjoint path sets. Overlap is a visible conflict; the kernel never invents an unreviewed semantic
merge.

M7 already proved each Proposal's patch equals its complete sealed sandbox diff. The prepared
candidate therefore retains that sealed final Manifest. Integration composes verified Manifest
entries over the exact Proposal Base rather than reparsing a patch. It seals an immutable
`Capture::Derived` Snapshot, materializes that exact Manifest in a fresh sandbox, and runs the
captured required post-apply checks before promotion. A failed check leaves the current Campaign
head byte-identical while retaining the prepared Snapshot and check evidence.

Promotion is internal. After optimistic revalidation of the expected Subject, Finding and Demand
views, selected receipts, policy, and semantic-closure record, one event-store transaction records
the applied Proposals, the derived Subject/head, and Change Attestations that move covered claims
to `pending-verification`. The next Round consumes that internal head, resets the clean window, and
runs the full required graph. Only the existing Fix Verification path may resolve those claims.
Publishing a branch or PR remains outside the kernel.

## Considered options

- **Apply patch text directly to the live checkout.** Rejected because filters, hooks, partial
  writes, and external branch state would cross the internal authority boundary.
- **Ask a model to merge overlapping Proposals.** Rejected because its output would be a new,
  unreviewed change with none of the sealed Proposal proofs.
- **Advance the head before post-apply checks finish.** Rejected because readers could observe a
  candidate that policy has not admitted.
- **Compose verified sealed Manifests and atomically promote only the checked derived Snapshot
  (chosen).** It reuses M7's exact-diff proof, handles binary content, and leaves one replayable
  visibility boundary.

## Consequences

- `auto_apply` is a bounded Execution Binding capability, not a reviewer suggestion and never an
  implicit default.
- Conflict, preparation, check, and commit outcomes are durable even when no promotion occurs.
- Automatic Integration never writes Git branches, the worktree, remotes, or credentials.
- A committed Integration cannot itself make a Campaign converge; verification requires a later
  full Round over the derived head.
