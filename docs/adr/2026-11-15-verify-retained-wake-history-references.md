# ADR-2026-11-15-verify-retained-wake-history-references: Verify retained wake history

**Status:** proposed (2026-11-15); candidate repair for #952.

## Context

A genuine new cancellation changes authenticated run facts and requires opening replay. The
shared retained-state check introduced for #948 validated `through` but omitted the other source
reference, `lease_boundary`, and trusted the checksum-consistent old `prefix_digest`. Rebuilding
could therefore erase retained source corruption. A checksum proves consistency of control bytes;
it does not establish their relationship to canonical Task history.

## Considered options

- Refuse all new cancellation facts: rejected; legitimate lifecycle progress must remain possible.
- Check only that the lease event exists: rejected; a real non-lease or stale lease is insufficient.
- Duplicate the projector's fingerprint algorithm: rejected; separate definitions could diverge.
- Share the canonical fingerprint and lease-boundary rules in the Store: selected.

## Decision

Before admitting retained continuation, replay the exact covered source in existing bounded pages
(4096 rows, 1 MiB per row, 16 MiB buffered). Verify both sequence/event pairs, the latest
Opened/LeaseTaken/LeaseRenewed/LeaseReleased boundary at or before `through`, and the exact rolling
fingerprint seeded by the registered Task identity. The projector and retained validator use the
same hash and lease-role helpers. Existing canonical Task/CAS validation remains required.

The shared serialized reconciliation path performs this validation before facts comparison,
at replacement and during narrow pre-native deferral. Actual current checkpoint metadata and
provenance remain required even if checkpoint text is unchanged. Invalid retained authority
refuses without replacing checkpoint/progress or creating outbox, transport or native intent.
Facts-derived semantics are rebuilt from opening only after retained source integrity is proved.
No persisted shape, schema, authority, retention horizon, allowance or native resend fence changes.

The [field and boundary inventory](../design/task-wake-journal.md#retained-history-integrity)
distinguishes source identities, structural checks and prior-facts-derived state. Deterministic
public CLI tests retain a checkpoint before genuine cancellation, substitute checksum-consistent
corruption, and exercise initial inspection, replacement and real-writer contention deferral.

The accepted research Document is
`sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`,
source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`.
Its common Store/outbox recommendation and conservative uncertainty policy are preserved;
reviewed comparison base remains `1a4dd4cc82d5f01f55014283c95d3bf6ea77fac3`.

## Consequences

Retained integrity requires bounded-memory replay of the covered prefix, including after changed
facts; this is not constant-time replay. A valid older checkpoint still rebuilds and later records
authoritative completion on the same attachment. Independent whole-candidate verification and
Pi/Codex live certification remain coordinator work; Claude live trials remain deferred under #876.
