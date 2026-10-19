# Design notes

The designs that still describe work in flight. They are direction, not an inventory of
shipped behaviour: the ADRs under [`../adr/`](../adr/) are the binding record, and where a note
and an ADR disagree, the ADR wins. Shared vocabulary is defined in
[`CONTEXT.md`](../../CONTEXT.md); the engineering values that order trade-offs are in
[`../values.md`](../values.md).

- [`worker-warm-layers.md`](worker-warm-layers.md) — splitting a Worker Attempt into carried
  layers (Notes, Head Delta, workspace, build cache) so later Rounds do not pay the cold start
  again. Packages P1–P3 are accepted as
  [ADR-0107 *Carry Worker Notes*](../adr/0107-carry-worker-notes-and-head-deltas-as-declared-warm-layers.md)
  through [ADR-0109](../adr/0109-rebase-warm-workspaces-at-stable-roots-with-digest-verification.md).
  P4, the session layer and Cold Closeout, never ran on the Task host and was removed by
  [ADR-0145](../adr/0145-read-only-what-this-release-writes-and-version-every-changed-shape.md);
  nothing further is planned.
- [`worker-warm-layers-plan.md`](worker-warm-layers-plan.md) — the package sequence for that
  design, and the Findings each package had to prove. The Task files under `.af/tasks/warm-layers/`
  name it as their plan.
- [`self-optimizer.md`](self-optimizer.md) — the design behind `af self optimize`. Milestones
  M1–M3 ship; the M4 heavy whole-Pipeline redesign in §10 has not started.
- [`research-pipelines.md`](research-pipelines.md) — research through `af`: warm Task checks,
  kernel-run measurements and comparisons, a report Task profile, bindings for every declared
  root port, and `af task gc`. Packages R1–R5 are accepted as
  [ADR-0131](../adr/0131-warm-task-checks-through-a-toolchain-keyed-bounded-cache.md) through
  [ADR-0135](../adr/0135-collect-finished-tasks-behind-a-tombstone-and-a-reachability-sweep.md),
  and the R6 campaign closed with its report verified; the report itself and its generated
  sources file were removed from the repository, and git history keeps them.
- [`remote-checks.md`](remote-checks.md) — a declared Task check run by a remote executor (a
  draft pull request and the repository's CI) and bound to the exact Snapshot, chosen by the
  pipeline's check node, for hosts too small to build the project. Package RC1 is accepted as
  [ADR-0140 *Run a declared check through a gate pull request*](../adr/0140-run-a-declared-check-through-a-gate-pull-request.md),
  and package RC3, which moves the choice from the machine to the pipeline, as its amendment of
  2026-10-05; the live proof is recorded in its §6, and adoption in this repository's policy
  follows the release.
- [`disk-budget.md`](disk-budget.md) — af holds a bounded, visible amount of disk: a stable warm
  cache key, one machine-wide budget evicted least recently used and enforced after every run,
  collection that reaches every Store, an af-owned scratch directory and `HOME` for every check, and removal of
  what Workers and gates leave in Claude and GitHub. Accepted 2026-10-07; package D1–D5 in flight.
- [`project.md`](project.md) — one **Project** entity per repository (identified by the root
  commits every Snapshot already carries), its **Workspaces** (one registered checkout per
  workstream, adopting the `workstreams/` habit), kernel-measured **Surveys** of tree, history,
  contributors, decisions and af activity recorded by a token-free **Surveyor** at onboarding and
  after runs, and bounded **Project Briefs** that a pipeline carries to chosen Workers on a
  declared port; all behind `af project`. Proposed 2026-10-19; packages PJ1–PJ5 not started.
