# Make review selectors explicit and refuse empty Diffs

**Status:** accepted (2026-09-01)

A diff review has three independent selectors: the policy revision that supplies trusted `.af/`
authority, the Base Snapshot against which the change is measured, and the candidate revision or
explicit dirty worktree. `af review plan` resolves all three without opening a Campaign, running a
Gate, admitting a Provider, or dispatching a Worker. `af review run` prints the same resolved
subject summary before any external work.

New invocations spell the selectors as `--policy-rev`, `--base`, and either `--candidate` or
`--uncommitted`. The compatibility spelling `--authority REV` expands visibly to
`--policy-rev REV --base REV`; it cannot be mixed with either explicit selector. A diff whose
typed Change Set is empty is refused during preparation, before any Gate, Provider operation, or
Worker dispatch. Intentional whole-tree review remains an explicitly selected whole-tree pipeline
rather than an empty Diff convention.

## Considered options

- **Keep one `--authority` revision as both policy and Base.** Rejected because the overloaded
  spelling hides two different trust decisions and makes reviewing a commit against its parent
  unnecessarily error-prone.
- **Treat an empty Diff as a clean review.** Rejected because no Worker inspected a change and a
  false clean result can cross a merge gate.
- **Dispatch a Worker and ask it to notice that the Diff is empty.** Rejected because the kernel
  already knows the exact Change Set and must not spend tokens to recover a deterministic fact.
- **Resolve three explicit selectors in a token-free plan and refuse empty Diffs before external
  work (chosen).** This makes the intended authority and Subject inspectable and gives both CLI
  and automation one fail-closed path.

## Consequences

- Policy and Base may name different revisions in the same repository and are captured as
  distinct immutable Snapshots.
- Plan output contains original selectors, resolved revisions and Snapshot IDs, changed paths,
  rename evidence, pipeline topology, budgets, Gates, and Provider requirements.
- A typo that resolves candidate and Base to the same tree is an error, not a successful clean
  Campaign.
- The compatibility alias remains available but always exposes its exact expansion.
