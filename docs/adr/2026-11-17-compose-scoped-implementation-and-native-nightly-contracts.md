# ADR-2026-11-17-compose-scoped-implementation-and-native-nightly-contracts: Combined Scenario contracts

**Status:** proposed (2026-11-17).

## Context

The verified issue 834 candidate at `211289e47988bcbe5a7322f8efc58e55424bdb8d`
and merged main PR1037 at `42d5b0152aa270df39c2b989b9d5e8477d6a84df` extend the
same closed Scenario contracts independently. PR1037's exact patch starts at
`3ddf026579b1fd2acaf2716cdac2a5fae1a4d93f`; its SHA-256 is
`66dcde18e50ca96a5ec037e57ac8ba5e637876375148572066df6c7029bf3e59`.
Neither parent's contract identity describes the combined shape.

## Considered options

- Replace either parent: rejected; this loses required implementation or nightly behavior.
- Reuse a parent's version or add aliases: rejected by ADR-0145; old evidence must fail closed.
- Widen implementation startup authority to nightly work: rejected; release permission has its
  own explicit machine scope and external signing prerequisites.

## Decision

Compose the native nightly scalar, projection, Actions, predecessor observations and scheduled
local-date Ant inputs with scoped implementation startup admission, terminal native repair
outcomes, independent publication proof and retained bounded driving. Use new closed versions:
Scenario/Bundle 9, Policy 8, Pin/Run/List 10, Store 15, adapter request 7/response 3,
Ant 10, Ant Pin/Tick 11, State/List 12, inspection 7, workspace state 5 and GitHub state 8.
Current writers, readers, schemas, fixtures and release declarations agree. Retired shapes are
refused; historical Stores, plans and captured evidence are never rewritten.

This supersedes only the version declarations affected by composition in the
[nightly decision](2026-11-16-orchestrate-native-nightly-releases.md) and the scoped
implementation decisions. It adds no departure from ADR-0274 beyond the already scoped
accepted-result repair observation, implementation publication decision and retained Wait cadence
clauses described in the [active-frame decision](2026-11-17-bound-implementation-driving-to-active-frames.md).
Generic exact signed decisions remain available. Merge remains exclusively external.

Preserve original finite budgets, repair counts, progress credits, 30000 ms default idle cadence,
expiry and deadlines. Completed child frames remain evidence, excluded from the active deadline
frontier; their journal-reopen regression remains required. Keep fresh final publication guards,
exact cumulative accounting and reconciled effect identity. Nightly releases retain external
signing permission, remote asset verification and uncertain-publication reconciliation.
No signing key is retained by the Scenario driver.

Canonically regenerate the release capability declaration and protected harness inventory from
combined bytes. Their digests are candidate metadata, not authority to release or deploy.
The unrelated unequal-workload advisory benchmark in issue 1057 remains unchanged.

## Consequences

Focused isolated tests cover native implementation repairs/publication/restart and native nightly
admission, delayed completion, interrupted jobs and uncertain remote publication. Structural parity,
retired-version refusals and release controls check the combined contract closure. These use
controlled Providers, local gates, disposable repositories and fake remote IO; they establish no
paid Scenario execution, live publication, physical VM reboot or mid-Worker termination claim.
The kernel runs its full configured fast gate after candidate sealing; independent original-goal
verification remains a subsequent native Task. No support candidate for issues 815 or 805 is
imported by this integration.
