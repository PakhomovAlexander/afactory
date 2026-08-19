# Case: a defect that spans two individually valid slices

**Phase 4.** Discharges: *"Dynamic scatter requires a whole-target closeout, including a fixture
whose defect spans two individually valid slices"* and *"Scatter persists a complete SliceSet
before fan-out and exposes every missing shard."*

## Why there is nothing to capture

Splitting a large diff by subsystem is currently a rule the orchestrating agent follows by
hand, and nothing records the partition or checks what it hid. The failure mode is structural,
so it needs a fixture the moment scatter becomes a real node.

## Setup

A change touching two subsystems that a sensible planner puts in different slices:

- **Slice A** — a producer that starts emitting a new field, and stops emitting an old one it
  documents as deprecated. Internally consistent; its tests pass.
- **Slice B** — a consumer updated to read the new field. Internally consistent; its tests pass.

The defect exists only in their interaction: the consumer's rollout precedes the producer's, so
between deploys the consumer reads a field nobody writes. Neither slice is wrong on its own,
and a reviewer restricted to either one is *correct* to report nothing.

Add one shard that fails to produce output at all (killed mid-attempt), to prove the gather
boundary.

## Required behavior

- The `SliceSet` is persisted **before** any shard starts, with each slice's identity, paths,
  commits and goal — the partition is evidence, not an implementation detail.
- Every shard reviews the same immutable snapshot and gets its own sandbox and output artifact.
- The dead shard is **visible** at gather: `require: all` cannot silently succeed with a missing
  shard, and the run must not converge as though that slice were reviewed and clean.
- A **whole-target closeout** node inspects the change as one unit and reports the interaction
  defect. This is the case that proves closeout is load-bearing rather than ceremonial: every
  per-slice reviewer returning "no findings" must not, by itself, produce a ship verdict.
- Slice identities are collision-free tagged tuples — a slice literally named `all` cannot alias
  the closeout node or its receipts.
- Replay with the shards completing in any order produces the same expanded graph, the same
  SliceSet ID, and the same verdict.

## What failure looks like

The most expensive kind: a clean report. Every reviewer is honest, every slice is green, and
the defect ships. A partition that cannot see an interaction has quietly redefined what "the
change was reviewed" means.
