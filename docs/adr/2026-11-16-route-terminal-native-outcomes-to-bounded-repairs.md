# ADR-2026-11-16-route-terminal-native-outcomes-to-bounded-repairs: Route terminal native outcomes to bounded repairs

**Status:** proposed (2026-11-16).

## Context

Stage A admits the complete implementation template pool before paid execution. Generic Scenario
Task steps nevertheless require accepted results and turn ordinary failed gates or review requests
into terminal failures. Counting all fresh Tasks as repairs also misclassifies verification and
reporting. A rejected candidate is evidence for repair, never an accepted publication candidate.

## Considered options

- Admit unsuccessful receipts as accepted Tasks: rejected because publication consumes accepted Tasks.
- Retry the same native Task with diagnostic prose: rejected because it loses immutable source and
  findings provenance and cannot grant a fresh Task's authority.
- Reset a repair counter for each nested branch: rejected because verifier and CI paths would widen
  the owner's allowance, including on restart.
- Retain a separate terminal evidence type and plan bounded native continuations: selected.

## Decision

Add the `task_outcome` port carrying `af/scenario-task-outcome@1`. It names the exact terminal
Task, plan, revision, result, native outcome/cause, known charge, unknown-usage flag and available
check receipt, available candidate Snapshot (including an Attempt-wall partial tree) and reviewed continuation. Accepted `task` ports keep their existing validation.
Only the checked `accepted_task` projection converts terminal evidence into an accepted receipt.
Neither failed checks nor missing acceptance can pass that projection or publication admission.
Read-only discovery retains its existing accepted report contract even when its domain conclusion
records findings; that observation never authorizes implementation repair without unsatisfied acceptance.

Ordinary check failures and reviewer/evaluator changes requested are repairable only when exact
native continuation evidence exists, usage is known and execution did not exhaust its authority.
Preflight, Provider, cancellation, expiry, exhaustion and missing evidence do not authorize repair.
The host re-reads the result, accounting and continuation from the same native Store before
planning a fresh implementation Task. Native continuation binds the reviewed candidate and its
findings; requirements additionally retain the typed check/outcome evidence. Lost responses reuse
the same deterministic Task identity and exact recorded plan. Planning failures retain their
reservation conservatively; this change introduces no nondispatch refund.

The generated recipe unrolls a finite tree. Every native or CI repair edge decrements the same
remaining allowance. A required independently pinned whole-change verification Task runs before preparation;
its rejection uses that same allowance. Every declared template, including skipped branches, is
admitted at startup. Total declared Tasks, token reservations, time and dispatch bounds are computed
from the actual generated tree independently of repairs. Scoped execution additionally permits at
most one initial implementation plus `max_repairs` implementation reservations across all frames;
verification and report Tasks consume total bounds but no repair slots. Reservation keys survive
restart and repeated dispatch does not acquire another repair slot.

This supersedes only ADR-0274 clause 2's accepted-only Task observation for explicit terminal
ports and the CI-only repair description in the implementation guide. Accepted Task contracts,
exact signed generic publication, one-observation-per-drive Waits, native Attempt caps and Task
sandbox/delivery boundaries are unchanged. Persisted containing shapes take new versions; old
Stores and definitions fail closed and are never migrated or rewritten.

## Consequences

This is Stage B of issue 834, built on Stage A. Stage C scoped publication and a continuous driver,
and Stage D complete lifecycle certification, remain pending. Independent whole-change verification
must use the original base; a continuation's own review otherwise covers its continued candidate.
No merge permission, retained signing key, synthesized signature, direct model dispatch or live
publication is added. Isolated command-backed lifecycle tests and transport mocks establish only
the paths their assertions execute; they do not prove paid Provider operation, VM reboot,
mid-Worker kill, continuous service operation or live GitHub publication.
