# Campaign authority is resolved before candidate capture

**Status:** accepted (2026-08-20)

A Campaign first resolves an operator/administrator-selected Authority Snapshot, then loads and
pins its pipeline, reviewer lock, reviewer packages, project policy, execution bindings, and
convergence policy from that trusted content. Candidate capture happens only after this immutable
campaign manifest exists. For a diff Subject the Authority Snapshot normally also serves as the
pinned Base; whole-tree Subjects still require explicit authority even though they have no Base.

## Considered options

- **Load `.review/` from each Round's candidate checkout.** Rejected because candidate code could
  grant itself commands or cache access, replace reviewer prompts, or change convergence between
  Rounds while retaining the same Campaign identity.
- **Declare the base ref inside the candidate pipeline.** Rejected as circular: the kernel would
  have to trust candidate configuration before it knew which content was trusted authority.
- **Resolve authority first and pin the manifest (chosen).** Mutable refs remain selector inputs;
  every authority-bearing byte is content-pinned before candidate execution.

## Consequences

- `CampaignOpened@1` stores the Authority Snapshot and all resolved authority-bearing digests.
- Campaign continuation uses the stored manifest and rejects a different pipeline, package set,
  focus, or policy. Intentional changes start a new Campaign.
- Secret values are never stored in the manifest; it records grant and execution-policy
  identities sufficient to explain and reproduce selection.
