# Design notes

The designs that still describe work in flight. They are direction, not an inventory of
shipped behaviour: the ADRs under [`../adr/`](../adr/) are the binding record, and where a note
and an ADR disagree, the ADR wins. Shared vocabulary is defined in
[`CONTEXT.md`](../../CONTEXT.md); the engineering values that order trade-offs are in
[`../values.md`](../values.md).

- [`worker-warm-layers.md`](worker-warm-layers.md) — splitting a Worker Attempt into carried
  layers (Notes, workspace, caches, session) so later Rounds do not pay the cold start again.
  Packages P1–P4 are accepted as
  [ADR-0107 *Carry Worker Notes*](../adr/0107-carry-worker-notes-and-head-deltas-as-declared-warm-layers.md)
  through [ADR-0110](../adr/0110-capture-sessions-in-two-phases-and-confirm-clean-rounds-cold.md);
  the session layer is being ported to the Task host.
- [`worker-warm-layers-plan.md`](worker-warm-layers-plan.md) — the package sequence for that
  design, and the Findings each package had to prove. The Task files under `.af/tasks/warm-layers/`
  name it as their plan.
- [`self-optimizer.md`](self-optimizer.md) — the design behind `af self optimize`. Milestones
  M1–M3 ship; the M4 heavy whole-Pipeline redesign in §10 has not started.
- [`research-pipelines.md`](research-pipelines.md) — research through `af`: warm Task checks,
  kernel-run measurements and comparisons, a report Task profile, bindings for every declared
  root port, and `af task gc`. Proposed 2026-09-27; packages R1–R6 are not yet delivered.
- [`remote-checks.md`](remote-checks.md) — a declared Task check run by a remote executor (a
  draft pull request and the repository's CI) and bound to the exact Snapshot, chosen by the
  pipeline's check node, for hosts too small to build the project. Package RC1 is accepted as
  [ADR-0140 *Run a declared check through a gate pull request*](../adr/0140-run-a-declared-check-through-a-gate-pull-request.md),
  and package RC3, which moves the choice from the machine to the pipeline, as its amendment of
  2026-10-05; the live proof is recorded in its §6, and adoption in this repository's policy
  follows the release.
- [`disk-budget.md`](disk-budget.md) — af holds a bounded, visible amount of disk: a stable warm
  cache key, one machine-wide budget evicted least recently used, collection after every run that
  reaches every Store, an af-owned scratch directory and `HOME` for every check, and removal of
  what Workers and gates leave in Claude and GitHub. Accepted 2026-10-07; package D1–D5 in flight.
