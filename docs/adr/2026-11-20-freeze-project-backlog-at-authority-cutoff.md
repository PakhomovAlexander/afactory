# ADR-2026-11-20-freeze-project-backlog-at-authority-cutoff: Freeze Project backlog batches at authority cutoff

**Status:** proposed (2026-11-20), accepted on reviewed merge.

## Context

The [private backlog intake decision](2026-11-20-store-project-backlog-reports-privately.md)
gives each accepted Report one sequence in the Project Store. Daily processing needs a stable
view of that intake and a way to resume even when the process that admitted the batch exits
before accounting for its members.

## Considered options

- Rescan the inbox on each retry. This changes the work when new Reports arrive.
- Admit an Ant in a separate Store and then copy its cursor into the Project Store. A crash
  between writes can consume an occurrence without freezing its batch.
- Keep local occurrence admission and the batch journal inside the Project Store lock and
  reuse the Ant scheduler's deterministic pin, calendar and admission rules.

## Decision

1. A machine-local policy pins the Project, designated intake authority, Scenario identity,
   daily calendar and timezone transitions, evidence access, run and rolling allowances,
   destination, publication cap and hold policy. A separately installed policy change fails
   closed on tick. Operators can invoke the local entry point with an explicit clock.
2. A tick freezes the committed authority high-water mark and explicit membership under
   the same Project Store lock that sequences intake. The journal records both in one
   transaction. Frozen work includes new Reports, carried deferrals and explicit eligible
   reconsideration requests. Admission limits membership to 128 and bounds the serialized
   context. Excess work remains queued and keeps its original sequence. A request can link a later authority-accepted Report as
   evidence. Its eligibility time is compared with the scheduled occurrence.
3. The local journal retains occurrence and disposition history. While a batch has unfinished
   members, another due occurrence is skipped as busy. Closing requires a durable disposition
   for every member. Capacity deferrals stay visible and carry forward on close, releasing the
   batch without losing the deferred work. Identical retries recover the same recorded effects.
4. Only one active batch may reserve a given item for the pinned destination. The hold policy
   can refuse all local reservations. A reservation represents a slot within the batch cap.
   It creates no publication or independent-verification record and releases on close.
5. Nonempty local batches reserve the per-run token allowance through the shared rolling-window
   rule starting at close. The reservation is conservative bookkeeping. The local commands
   use no Provider and record no measured spend. The audit includes Report identities,
   both cutoffs, occurrence dispositions, frozen members and reconsideration requests.
   Policy can opt into a private assessment view of report facts, reproduction and evidence
   excerpts. Batch audit retains only identities and digests for that evidence.

## Consequences

The local checkpoint can be exercised entirely with disposable Stores and an opaque destination
identity. Portable Scenario execution, evidence delivery to Workers, credential binding, external
publication and live Ant activation require their separately approved workflows.
