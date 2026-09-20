# Design notes

Design notes for Afactory — the `af` CLI and the kernel behind it. They describe intent and
vocabulary: the values that order trade-offs, the entity model, the lifecycles, where state
lives, how configuration layers, and the Task execution product model. They are direction, not
an inventory of shipped behaviour. The ADRs under [`../adr/`](../adr/) are the binding record;
where a note and an ADR disagree, the ADR wins. Shared vocabulary is defined in
[`CONTEXT.md`](../../CONTEXT.md).

- [`values.md`](values.md) — the engineering values in priority order, plus three product values, each with its acceptance tests.
- [`overview.md`](overview.md) — the architecture direction: what `af` is, the rules the values impose, entities at a glance, decisions D1–D19, and the v1/v2/v3 cut.
- [`entities.md`](entities.md) — the six authored and six recorded entities, one section each.
- [`state-machines.md`](state-machines.md) — the Task, Pipeline, and Worker state machines and the event vocabulary they consume.
- [`config.md`](config.md) — the TOML precedence ladder, project and user files, the lock, and substitution.
- [`store.md`](store.md) — placement of the Store and the invariants its design must keep.
- [`research.md`](research.md) — the research digest the design borrowed from, with sources.
- [`task-execution.md`](task-execution.md) — the Task execution increment: Pipelines versus Execution Plans, selection before generation, bindings, catalog, and verification.
- [`task-execution-examples.md`](task-execution-examples.md) — proposed TOML examples and conformance cases for that increment.
- [`worker-warm-layers.md`](worker-warm-layers.md) — proposal: split a Worker Attempt into carried layers (Notes, workspace, caches, session) so later Rounds do not pay the cold start again.
- [`worker-warm-layers-plan.md`](worker-warm-layers-plan.md) — implementation plan for the warm layers: packages P0–P4, the campaign Pipeline every package runs through, and the review Findings each package must prove.
