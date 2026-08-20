# Reviewers author patch proposals; the kernel verifies, git applies

**Status:** superseded by
[ADR-0010](0010-proposals-are-exported-by-id-and-base-bound.md) (2026-08-20)

`PatchProposal@1` is fully typed in `review-core`, has a schema and parity tests, and has zero
call sites in `review-pipeline` or `reviewctl`. The seal layer derives what a node changed
specifically so a proposal can be checked against reality, and the README states the rule that
derivation exists to enforce: "a proposal must equal the kernel-computed diff, so an unreverted
debug probe fails it rather than riding along". Nothing produces a proposal, so nothing enforces
anything.

We decided to wire it. Reviewers are asked for a patch alongside each finding; their sandbox is
already `Mode::EphemeralWrite`, so they edit it to demonstrate the fix; sealing computes what
actually changed; and a proposal that does not equal that diff is refused rather than stored.
The proposal attaches to its Finding as an artifact.

Getting it into a working tree is a separate step and stays outside the kernel:
`reviewctl export <key>` emits the patch on stdout and the operator pipes it to `git apply`.

## Considered options

- **Record it as deliberately dormant.** Note in `review-core` and the README that the contract is
  designed and unwired, keep the types as the agreed shape, revisit later. Cheapest, preserves
  optionality, and there is a real argument for waiting: a proposal scoped to a Change Set is
  worth far more than one scoped to the whole tree, so this is more valuable after M3. Rejected
  as too passive given how much of the machinery already exists.
- **Delete `PatchProposal` and the seal commentary serving it.** Smallest maintained surface, and
  it would make the code agree with SKILL.md's "you are the only fixer" everywhere. Rejected: it
  throws away a designed contract, including one of the three properties the README lists as
  deliberately unrepresentable ("a patch proposal cannot name zero claims").
- **Wire it, with `reviewctl apply` writing the working tree.** Best ergonomics, and the kernel
  knows things git does not — which snapshot the proposal was verified against, and which claims
  it covers. Rejected for the boundary it costs; see below.
- **Wire it, applying onto a branch cut from the reviewed snapshot.** Staleness becomes impossible
  by construction, since the patch is applied to exactly the tree it was computed from, and the
  working tree is untouched. Rejected for this round: it still writes refs and objects, so the
  invariant still needs rewording, and it produces one branch per accepted proposal.
- **Wire it, with `reviewctl export` and `git apply` (chosen).** Keeps `main.rs`'s claim — "Nothing
  here mutates any repository" — true verbatim, with no exception to maintain. Git handles
  staleness better than the kernel could: it fails loudly when the patch no longer matches, offers
  three-way merge, and the operator sees the patch before it lands.

## Consequences

- SKILL.md's boundary is unchanged and its wording turns out to be exactly right: reviewers never
  edit *anything that is integrated*. Sandbox mutation already happened; only the kernel
  integrates, and now integration is a human running `git apply`.
- Reviewer rounds get more expensive. A model that writes code costs more than one that writes
  prose, and the `[budgets]` caps in every pipeline need revisiting when this lands.
- A proposal that does not equal the sealed diff is refused, not stored — so the failure mode is a
  missing proposal, never a proposal carrying an unrelated edit.
- A reviewer now authors code intended for application, which raises what a compromised reviewer
  can reach. Combined with [ADR-0003's](0003-gate-caches-pass-through-to-the-host.md) host cache
  passthrough, the risk delta is recorded under M6 in [`../backlog.md`](../backlog.md); the
  containment probes stay open and labelled open rather than narrowed away.
- Sequencing: this should follow M3. A proposal against a whole-tree Subject has far less to say
  than one scoped to a Change Set.
