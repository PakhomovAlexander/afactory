# ADR-2026-11-15-validate-retained-wakes-before-rebuild-and-fence-every-clock: Validate wake rebuilds

**Status:** proposed (2026-11-15); candidate repairs for #948 and #949.

## Context

New authenticated cancellation facts require rebuilding lifecycle semantics from opening. The
previous journal checked retained configuration only when restoring with matching facts, so a
checksum-valid foreign checkpoint could be replaced on the rebuild path. Live contention also
sampled a fresh serialized clock without comparing it to the latest post-source sample.

## Considered options

- Refuse every change in cancellation facts: rejected; legitimate cancellation must progress.
- Compare only the initial and post-source clocks: rejected; serialized validation samples again.
- Share serialized retained-state validation and carry the latest live sample: selected.

## Decision

Validate the actual retained subscription, configuration, opening and registration boundary,
checked source/CAS, checkpoint boundary and retained provenance, and stored progress independently
of current run facts before selecting restoration or replay. Use the same validation inside the
page commit transaction and the pre-native deferral transaction. The expected checkpoint remains
an exact compare-and-swap fence, so a concurrent replacement must pass current validation too.
No checkpoint is compared with a stale initial live clock by this integrity check.

Carry the latest sampled live clock through projection into serialized validation. A fresh sample
below it is terminal, even when every event and checkpoint predates the rolled-back clock. Equal
and increasing samples still require all existing current timestamp, range, expiry, authority and
recorder checks before bounded deferral. Explicit-clock projection remains strict and unchanged.
No stored shape, schema, poll allowance, retry policy or native uncertainty fence changes.

The [boundary and evidence map](../design/task-wake-journal.md#retained-rebuild-and-clock-monotonicity)
documents the normal and deferred paths. Deterministic public CLI fixtures publish genuine
cancellation and completion records; barriers and private debug clocks determine interleavings.

Research Document `sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`,
source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`,
and its common Store/outbox recommendation and conservative ambiguous-admission policy remain
unchanged. The reviewed comparison base is `1a4dd4cc82d5f01f55014283c95d3bf6ea77fac3`.

## Consequences

Valid old checkpoints can rebuild after new cancellation facts; invalid retained authority cannot
be erased by rebuilding. Clock rollback cannot become a successful empty Pi response. Independent
whole-candidate verification and final Pi/Codex live certification remain coordinator work;
Claude live trials remain deferred under #876. No new execution or publication authority is added.
