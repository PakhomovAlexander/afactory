# ADR-0062: Refresh issue revisions without resetting execution authority

**Status:** accepted. Superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): the
original ownership scheme and recovery kept for historical receipts without a result field.

## Context

A changed issue must invalidate an affected plan and its approval. Restarting accounting would
let repeated edits buy unlimited implementation or consume the verifier reserve. Recording a
revision and a replacement plan in separate transactions would leave an ambiguous crash gap.

## Decision

`af task refresh` explicitly captures the original issue selector again, without executing a
Worker. It uses the captured Task definition and structured specification; live project edits
cannot change code inputs, policy, verification, facts or limits. Local refresh reads the
original project path or an explicitly supplied JSON/TOML file. Project-relative capture opens
regular files beneath a held directory without following symlink components. Jira retains the
exact tenant, issue key and selected field set; changed local account settings cannot redirect
capture to another issue. Source retrieval happens outside the writer lease.

Compare external identity, revision label and selected value/text identities. Changed selected
bytes create a revision even under an unchanged upstream label. Formatting and unselected field
changes alone retain the existing observation, plan and approval. Resume never fetches a source.

Acquire the common fenced lease, verify the predecessor again and settle lost Attempts before
selection. Reuse the runtime's heartbeat and Store connection while restoring authority and
compiling. Select using remaining capacity, retaining the original total limits in the revision
and plan. A fitting previously generated definition retains its recorded Planner provenance and
needs a fresh exact developer signature. A permitted no-fit enters the fixed preparation plan;
refresh does not execute that Planner either.

One `SourceRefreshed` event records the new revision and either the exact plan or an explicit
waiting reason. The Store independently validates the issue identity, originating definition,
normalized selected fields and unchanged execution authority. It clears active invocations and
outputs and marks the replacement unadmitted. It retains all Attempts, reservation identities,
late usage, prior results, decisions and delivery history. Reinstalling a graph restores the full
verification reserve on the same ledger. Prior approvals cannot authorize the new plan.

Selection failure records the changed request with `needs_input`, `needs_human` or
`needs_resources`. Resource feasibility is checked again at the atomic barrier; exhausted or
expired capacity records a waiting revision rather than leaving an old approval current. Lease,
predecessor, pending-work and invalid-source conflicts still fail without that transition.
`af task run` on an unplanned waiting revision reports its state and exits 4 without dispatch.
Refresh grants no budget extension and does not alter an already delivered worktree. An unresolved
prepared delivery must be reconciled before refreshing; losing its process lease does not erase
that pending filesystem operation.

New common-Task delivery receipts carry the exact result ID. Delivery journals and Git ownership
refs are scoped to that result, so a later verified revision can be delivered to another absent
branch/worktree while preserving earlier deliveries. Inspection shows delivery only for the current
finished result. Historical receipts without a result field keep their original ownership scheme
and recovery behavior. The existing clean-target, source-identity and explicit-confirmation rules
apply to every result.

## Evidence

Real CLI fixtures refresh an approved generated issue, reject its old signature, retain one paid
Planner Attempt and execute only after fresh approval. Another fixture runs two issue revisions
on one eight-Attempt allowance, preserves S0 despite live code edits and records a third revision
as resource waiting. Store tests reject authority/input forgeries, retain late usage across replay
and record source waiting after a reported overrun. Budget fixtures cover pending work, backward
clocks, expiry, reservation identity and restoration of protected verification. The source reader
refuses symlink components, nonregular files and oversized input. Delivery fixtures cover pending
preparation before refresh and separate delivery of two verified revisions.
