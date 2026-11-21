# ADR-2026-11-20-prepare-private-daily-backlog-assessments: Prepare private daily backlog assessments

**Status:** proposed (2026-11-20).

## Context

The [batch journal](2026-11-20-freeze-project-backlog-at-authority-cutoff.md)
owns the authority cutoff, membership, evidence access and budgets. The
[portable report contract](2026-11-19-prepare-portable-scenario-artifacts.md)
owns receiver-specific Task preparation and independence. A portable batch assessment
needs both without copying intake data or grants into the artifact.

## Considered options

- Pass arbitrary named report sources. Caller-provided text cannot establish frozen membership,
  authority or cutoff.
- Package the producer's native template. This carries machine-specific paths and provider
  authority to other receivers.
- Resolve a typed batch reference in the receiving Project Store before preparing the native
  Report Task. Reuse its report checks and independent verification.

## Decision

Prepare the versioned backlog Scenario artifact with a typed Project, batch, policy and cutoff
reference. Check the active batch, current installed policy, committed artifact digest and bounds
under a shared Project Store lock. Capture the frozen identities and policy-permitted facts, reproduction and bounded evidence
excerpts as the private Report source for one read-only native Task. Both Workers receive the
same input. The tracked view binds verified claims and evidence to exact revisions and receipts.
Full attachments, signing keys and grants stay outside the captured input.

The report checks require a typed private assessment with exactly one decision per frozen
member, bound to the captured checkout revision. Evidence and citations must resolve through
the captured inputs. Verified decisions include exact proposed fields for the independent
verifier to check. The verifier judges factual persistence, grouping and privacy on the
native Task's exact Document and source Snapshot.

## Consequences

Receivers approve their own native template and Scenario pin. Preparing and resuming a private
assessment yields a traceable Report receipt. Project settlement and dispatch require their
own durable authority and remain separate from Report acceptance.
