# Design notes

The two designs that still describe work in flight. They are direction, not an inventory of
shipped behaviour: the ADRs under [`../adr/`](../adr/) are the binding record, and where a note
and an ADR disagree, the ADR wins. Shared vocabulary is defined in
[`CONTEXT.md`](../../CONTEXT.md); the engineering values that order trade-offs are in
[`../values.md`](../values.md).

- [`worker-warm-layers.md`](worker-warm-layers.md) — splitting a Worker Attempt into carried
  layers (Notes, workspace, caches, session) so later Rounds do not pay the cold start again.
  Packages P1–P4 are accepted as ADR-0107 through ADR-0110; the session layer is being ported to
  the Task host.
- [`worker-warm-layers-plan.md`](worker-warm-layers-plan.md) — the package sequence for that
  design, and the Findings each package had to prove. The Task files under `.af/tasks/warm-layers/`
  name it as their plan.
- [`self-optimizer.md`](self-optimizer.md) — the design behind `af self optimize`. Milestones
  M1–M3 ship; the M4 heavy whole-Pipeline redesign in §10 has not started.
