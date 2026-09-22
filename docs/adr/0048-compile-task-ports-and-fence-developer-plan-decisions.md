# ADR-0048 — Compile Task ports and fence developer plan decisions

**Status:** accepted for the unreleased Task increment, 2026-09-11. Implementation is in
progress; the common runtime and production authority adapter are not yet enabled. Superseded in
part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): the legacy-link obligation, and the
scheduler's own Gate suppression.

## Decision

Task Pipeline TOML is normalized before artifact capture, then admitted through the P01
contracts. Human-authored sets may be reordered into canonical order; duplicate entries
remain errors. Recorded artifacts always use strict deserialization, without normalization.

The pure Task compiler receives captured Pipeline definitions and trusted operator/Worker
signatures. A Pipeline slot cannot invent a Worker's role, protocol, port types, effects or
evidence authority. Compilation expands calls into qualified nodes while retaining the call
hierarchy and child resource caps. Child root applicability does not restrict embedding;
required child ports remain explicit and never acquire root defaults.

Conditional nodes consume one trusted receipt outcome. Conditions propagate through embedded
calls; required public outputs must exist on every enclosing path. Select has `condition`,
`passed`, `failed` and `inconclusive` inputs and one typed `output`. The three value arms have
the same contract. Select preserves the chosen value's Snapshot lineage and only retains
evidence authority common to every arm. A missing selected value is failed execution.

Task scheduling preserves ordered artifact vectors. Inactive branches are suppressed before
invocation publication. Optional physical Select inputs carry the inactive arms; they do not
relax the compiler's requirement that the chosen arm produces its value. Legacy review sorting
and Gate suppression retain their existing semantics.

Task packages use explicit `pipeline.toml` or `worker.toml` manifests. Capture checks the existing
lock resolver's digest over the same files that it parses. Captured packages are immutable CAS
envelopes. Admission reconstructs the plan from trusted package pins, installed signatures and
admitted Worker settings, then compares the graph, closure, bindings and acceptance coverage.
Validation neither consults mutable registries nor recreates a missing compiled artifact.

The existing graph planner and scheduler remain the only topology and scheduling mechanism.
Compiled Task nodes have an explicit Task kind; legacy review dispatch refuses that kind
unless the shared Task admission path is present. Snapshot lineage is checked separately from
legacy `SameSubject`: derived output has a distinct identity, and transitive ancestry does
not establish equality. Trusted Task-kind policy binds each acceptance obligation to its
required final public output. Evidence for a different Snapshot cannot satisfy that obligation.

Worker slot mapping qualifies independence constraints and rejects mapping independent slots
to one effective slot. Replacement also requires the child's permission. Public coverage must
be supported by trusted evidence retained in that public output. Composite evidence envelopes
need explicit installed retention semantics; a matching JSON type alone is insufficient.

Task lifecycle changes use additive `TaskTransition@1` events in the existing Store. The Task
namespace is domain-separated from Campaign IDs. The shared CAS publication barrier and
SQLite immediate transaction publish durable references before log rows. A Task write validates
its transition against the captured sequence, then compares that sequence inside the append
transaction. A racing writer or lease takeover invalidates the write. Generic and legacy append
entry points cannot append Task events or write Campaign events into a Task log.

Writer leases carry an owner, epoch and deadline. Their Rust handles cannot be deserialized
from Worker output. Time comes from the trusted host; persisted policy time is monotonic and
replay checks the original observations rather than today's clock. A projection cache uses an
append-only sequence watermark and revalidates active CAS identities before reuse.

The host implements a single Task authority boundary. It recompiles exact dependencies and
derives generated provenance, authenticates developer decisions, checks current authorization,
and derives acceptance from durable receipts. Public actor/authorization strings never create
that authority. The production implementation must be wired through captured project policy
and kept outside Worker sandboxes; a test authority is only a lifecycle test fixture.

Plan proposal, developer decision and execution admission are separate transitions. A generated
proposal persists its waiting state; approval does not dispatch. An exact repeated approval
returns its original event. Rejection, revocation, deadline expiry, changed Task inputs and
stale writer epochs refuse admission. New revisions cannot reset prior resource limits.

One Task budget extends the existing token ledger with Attempt-count and wall-time protection
for still-required verifiers. Preparation reserves before dispatch, started failures retain
their charge, and only a provably unstarted invocation can release its Attempt credit. An
observed per-Attempt overrun is fully charged and prevents new dispatch, including an already
prepared verifier. Replay reconstructs reservations and charges; child Pipelines use the same
ledger rather than creating their own.

## Remaining implementation obligations

Before the first PR is ready, complete production host authority wiring, legacy links,
common invocation and settlement, per-node Snapshot
materialization, and implementation/review cutover. Missing execution remains distinct from a
negative check result. General Task sealing retains a separate contract from frozen checked
Review Integration. These obligations are not satisfied by the new types or lifecycle mocks.

The complete package and PR sequence is tracked in [the delivery record](../task-execution/pr-sequence.md).
