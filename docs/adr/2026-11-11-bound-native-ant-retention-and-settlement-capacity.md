# ADR-2026-11-11-bound-native-ant-retention-and-settlement-capacity: Bound native Ant history and protect settlement

Status: proposed
Date: 2026-11-11

## Context

ADR-0274 and the native interval decision admit fixed-rate one-second Ants. Retaining every
occurrence, including busy/disabled skips, and every full Run eventually fills the existing
16 MiB Scenario journal. Refusing the next journal update also refuses settlement and recovery
of work already admitted. The preceding raw-Markdown and scheduler-stop repairs did not resolve
this recorded finding. Their evidence remains historical, not acceptance of this candidate.

## Options considered

- Raise the journal limit, disable machine storage admission, or require deleting a live Store:
  rejected; none bounds sustained use or preserves recovery authority.
- Move accounting into an auxiliary file or external scheduler: rejected; it divides ownership,
  replay and transaction authority.
- Keep every completed body forever: rejected; body size and event count both grow with time.
- Retain only the latest success: rejected; it would hide a newer unavailable result, uncertainty,
  charges, and skipped dispositions.
- Native transactional compaction with compact accounting and byte leases: selected.

## Decision

The Scenario Store is generation 8. Ant state is generation 7; Ant inspection is generation 2,
and Ant list is generation 7. Ant definitions/pins remain generation 6, and unchanged Scenario
contracts remain generation 5. Current writers, readers, ownership markers, schemas and parity
checks change together. Earlier Store/state generations fail closed without migration or fallback,
following ADR-0145. No old schema is shipped as an alternate reader.

Each successful Store update compacts Ant-owned history in the same immediate SQLite transaction
as journal and result-index writes. Keep the most recent 32 occurrence receipts, the latest
admitted Run even after later skips, and every receipt/Run with active or waiting work, unresolved
Task usage, prepared/uncertain intents, or unconfirmed child cancellation. Goal-claim Runs remain
retained for native roadmap replay. Manual Runs are not collected by this policy.

Older eligible completed Runs and skipped receipts expire. Their full bodies, named outputs,
decisions and diagnostic presentation are unavailable thereafter. Delete their keyed output-index
rows transactionally too; the index is not a second archive. A missing old output explicitly says
it may have been pruned. Ant output still selects the latest admitted Run, including an unavailable
new producer. It never substitutes an older success.

Every Ant exposes `retention`: exact retired occurrence counts by disposition (range counts are
individual scheduled indices), terminal Run count, cumulative charged tokens as canonical decimal
u128 text, highest retired index, retired mode-change count, and a content hash chain over expired
receipts/Runs and mode transitions. The hash is provenance, not retrievable evidence. Keep the
native monotonic `next_day` cursor and immutable pin; neither expiry nor restart resets dispatch
identity. Only mode transitions no longer needed for a future due decision may expire.

Keep at most 64 settled rolling-charge buckets. Sort by settlement time; merge the oldest buckets
at the later timestamp. This preserves exact decimal charges and can delay their expiry, never
release them early. Cumulative accounting never expires. Unresolved known-plus-unknown usage
remains on its original Run and holds its reservation regardless of age, terminal status or
clock changes. Unknown work is never put into expiring buckets. Rolling admission saturates only
its comparison at u64 maximum, while retaining the exact decimal sum.

`run_capacity` persists native byte leases in the World, keyed by the exact Run ID. Every read
checks lease identities, occupied payload sizes and aggregate capacity under the unchanged real
16 MiB limit. New admissions leave 512 KiB of transaction metadata space and reserve 1 MiB of
lifecycle capacity per admitted Run. Before an external dispatch is committed, enlarge that
Run's lease for the bounded response: native Task CAS envelope up to 8 MiB plus lifecycle space,
or adapter response/escaping, copied bound inputs and lifecycle space. Other admissions and
history cannot borrow this capacity. Capacity refusal is inspectable (`skipped_capacity` for an
Ant admission); a refused next transition dispatches no new effect. An existing open intent is
retained for reconciliation. A failed fresh planning operation without a recorded Task target retains its original intent,
inputs, token uncertainty and lifecycle lease; the stopped driver cannot receive a Task result,
so unused full-result capacity is released. Known terminal leases shrink, retaining room for wider exact native
charge counters and overrun diagnostics. Cancellation and numeric reconciliation consume the
original Run's lease. No lease is a token charge or execution grant.

A completed response body has a separate explicit capacity-expiry rule. If its full projection
cannot fit (including duplication into roadmap presentation), a response settling a previously
recorded open intent may lose its just-completed output body. Record its content digest and
unavailability in the intent evidence/step diagnostic, retain the typed Done intent and exact
charges, and terminate further Run dispatch with an explicit capacity failure. A cancelled Run
stays cancelled. Inputs, pending intents, native Task targets, unknown reservations, cancellation
receipts and prior authority are untouched. The failed producer cannot imply the body is available,
successful Scenario completion, or a completed roadmap goal. This rule makes already-admitted
settlement durable rather than rolling back its charge when presentation is too large. Ordinary
retained latest outputs remain available. No required open-effect evidence is compacted.

All bytes stay in the existing owned Store and SQLite journal/index. SQLite may reuse its bounded
high-water pages; pruning is not a claim that the file shrinks immediately. The machine Storage
Budget still inventories those files. No external ledger, daemon installation, publication,
credential use or change to the live recovery monitor is introduced.

## Consequences

Sustained successful/skipped interval history is bounded by retained recent receipts and protected
work, rather than elapsed ticks. Many unresolved Runs, manual Runs, goal histories or oversized
presentations can still exhaust *admission* capacity; af reports that condition while preserving
already admitted lifecycle authority. Pruned full evidence cannot be reproduced from a summary.
Rolling buckets can conservatively defer admissions longer than an exact uncompressed ledger.

Deterministic private tests cross the old journal failure point with mixed busy/missed/terminal
outcomes and over 100,000 held/busy ticks, check exact accounting and unknown reservations, restart,
index deletion, latest output and old-output unavailability, large-response settlement under
admission pressure, and explicit completed-body expiry with native roadmap accounting. Existing
one-second process/watch and controlled stop-race regressions remain required. Fresh implementer
results are recorded separately; the sealed-candidate gate and independent correctness evidence
remain acceptance authorities.
