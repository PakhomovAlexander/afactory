# ADR-0067: Project common Task selections into canonical Review

Status: accepted, 2026-09-12; the Task-runtime cutover it was a compatibility checkpoint of is
complete. Superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): the legacy Store's
selection from `AttemptAdmitted@1`. `TaskReviewResultSelected@1` is the only selection a receipt or
Proposal accepts.

## Context

Review receipts and Proposals require a selected Reviewer Result. The legacy Store derives that
selection from `AttemptAdmitted@1`; a common Task already selects and publishes its output on
its own ledger. Emitting another admission would introduce a second execution authority and
misrepresent accounting. Domain publication must also survive a crash after Task settlement.

## Decision

Capture `af/TaskReviewContext@1` after the real common Attempt reservation. It names the exact
Task invocation and Attempt, canonical Campaign, Round and node invocation, Subject and
Manifest, declared Reviewer inputs, rendered bytes and context manifest. The trusted adapter
must recompute context admission; serialized context does not authorize dispatch.

The compatibility operator has two singular typed output ports: its Reviewer Result and
`af/TaskReviewResultMetadata@1`. The latter retains the canonical flat result and provenance
identities and a closed Proposal disposition. Both output envelopes and their Task output
wrapper must belong to the actual common Attempt. Frozen Reviewer Result payloads remain flat.

After common settlement and output publication, the Store's trusted entry point derives
`TaskReviewResultSelected@1`. It verifies the common selected Attempt, exact published output,
admitted context, raw result identity and result contract. The caller cannot choose another
Campaign or Round. Ordinary append, including a copy of a valid selection event, cannot create
this authority.

The append transaction compares both the validated Task prefix and the Review prefix under one
SQLite writer lock. It also checks lease, plan and approval expiry, current Round, exact canonical
node invocation, pinned Reviewer output contract and absence of another selection. A different
writer, changed Task, superseded Round or duplicate publication cannot win a race with that
comparison. Publication rechecks authority after domain callbacks.

Canonical output receipts resolve their selected result through either the historical admission
or the checked Task selection. Task-backed Proposal publication must additionally match the
disposition already recorded beside that selected result. The Task retains all spend; the Review
selection event creates no reservation, Attempt or charge. Exact publication replay verifies
the captured artifacts again and returns without appending another event.

## Compatibility and remaining integration

Historical admissions, Reviewer Result payloads, receipts and Proposal identities are unchanged.
The Store connection is implemented and tested separately from the remaining legacy CLI cutover.
That cutover still requires its trusted context compiler and adapter, common broker currentness,
canonical replay/report integration, bounded owned Scatter and Round continuation on the
original Task allowance. This ADR does not claim those entry-point connections are complete.

Regressions cover pre-settlement and pre-publication refusal, revoked approval, wrong result
metadata and routing, forged generic append, exact receipt and Proposal disposition, idempotent
reopen, one retained charge and a competing Store advancing the Task before the transaction.
