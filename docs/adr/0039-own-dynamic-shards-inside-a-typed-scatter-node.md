# Own dynamic shards inside a typed Scatter node

**Status:** accepted (2026-09-01); superseded in part by
[ADR-0113](0113-ga-reads-only-what-ga-writes.md): pipeline format 1, one of the static formats it
lists, no longer loads.

Pipeline format v5 adds `slicer` and `scatter` node kinds without mutating the validated outer DAG
at runtime. A Slicer deterministically publishes one complete `SliceSet@1` for the exact Round
Subject. A downstream Scatter owns the resulting tagged shard sub-invocations. Every shard still
has a collision-free runtime node ID, its own Attempt lifecycle, sandbox, receipt, and node plus
fan-out budget accounting. The Scatter publishes one `ShardSet@1` containing every completed,
failed, or missing outcome; downstream gather, closeout, and Ledger logic consume that typed set
rather than guessing dynamic edges from ambient state.

`SliceSetAccepted@1` is appended before the first shard dispatch. Static validation proves the
declared Slicer-to-Scatter-to-closeout routes; Round finalization proves that every accepted Slice
has exactly one Shard outcome and that every selected semantic output reaches its authoritative
sink or an explicit trusted disposition. All shards are required by default. A closeout reviewer
receives the whole Subject and the complete Shard Set; only Authority-Snapshot policy can record a
visible waiver.

## Considered options

- **Rewrite the planned DAG after the Planner returns.** Rejected because replay would need to
  reconstruct a graph different from the captured definition and ordinary receipts would no
  longer prove which plan executed.
- **Predeclare the maximum number of empty shard nodes.** Rejected because empty/suppressed slots
  obscure coverage, waste configuration and make runtime identity depend on a positional ceiling.
- **Let a Scatter return only successful reviewer results.** Rejected because missing and failed
  shards would disappear at the gather boundary and could be mistaken for a clean review.
- **Keep one outer Scatter node and persist typed sub-invocations plus a lossless Shard Set
  (chosen).** It preserves a static authority graph while making dynamic work and omission
  explicit and replayable.

## Consequences

- Dynamic runtime IDs are tagged with both Scatter and Slice identity; string concatenation that
  can collide with a static node is refused before `SliceSetAccepted@1`.
- A Scatter may complete with an incomplete `ShardSet@1`, but semantic closure and the final
  verdict cannot pass unless trusted policy explicitly permits partial gather.
- The full Subject remains one reviewer input. A Slice narrows attention; it never replaces
  Subject authority.
- Pipeline formats v1-v4 retain their frozen static semantics and cannot acquire dynamic fan-out
  by upgrading the binary alone.
