# ADR-0265: Bind comparison partition execution to reused build artifacts

**Status:** proposed (2026-10-30).

## Context

A full release comparison partitions one frozen core plan's shards into sequential bounded
originals on the serial local backend. The first partition builds; every later one reuses that
partition's verified, immutable build. A shard reads its run's execution start and total
deadline from the build it runs (ADR-0170) and never resets them. A later partition that
reused the first one's `builds.json` therefore inherited the first partition's run deadline:
once the first original's `run_total_minutes` had passed, no shard of any later partition could
start, so the partition never made the whole family feasible. A caller's deadline only
tightens a run's deadline, and the build's run is the build's provenance, so neither can be
moved without falsifying a receipt.

## Considered options

1. Remove the run deadline for reused builds. Rejected: a later partition would run without a
   total bound, and its shards' evidence could no longer be checked against one.
2. Write a refreshed `builds.json` for each later partition. Rejected: it fabricates a build
   receipt for a build that did not happen and breaks every shard bundle's binding to the one
   build of the comparison.
3. Seal each later partition's own execution authority beside its unchanged build. Chosen.

## Decision

A later partition's run bundle holds `partitions.json`, an `af.perf-partition-execution/1`
document the executor writes before any shard starts: the comparison, the plan's digest, the
reused build's exact `builds.json` digest and its recorded run (unchanged provenance), and for
the partition its index, original, shards and its own run window, the plan's frozen
`run_total_minutes` from the partition's own execution start. The first partition carries
none; its shards run in its build's run.

- The runner reads a shard's run window from the sealed authority of its run bundle when it
  has one. It refuses, before anything starts, an authority that names another plan, build or
  build run, a window that is not the plan's frozen total, one that starts before the reused
  build ended, an authority changed after its bundle was sealed, or a shard the authority does
  not declare. Every normal deadline still applies: the window's total, the shard's sampling
  window and the original's own `run_seconds` as the caller's deadline.
- The comparison aggregate verifies each later partition's authority against the declared
  partition and the first partition's build and plan, and carries every included one in its
  assembled run bundle. Evaluation checks each shard against its own partition's window, the
  others against the build's run, and that no partition's shards ended after a later
  partition's window started. One corrected family judgment over every partition is unchanged.
- A run bundle without the authority is evaluated exactly as before (ADR-0170). The authority
  is a new versioned shape with its schema; there is no reader for anything else.

## Consequences

A later partition is bounded by its own window and the original that executes it, and the whole
family at unchanged pair counts and closing sentinels fits sequential originals. The reused
build's receipt and every shard bundle's binding to it stay exactly as recorded. A forged,
widened or wrong-partition authority starts nothing and, in an assembled run, makes the shards
it would cover integrity failures.
