# Separate product orchestration from the Review Kernel

**Status:** accepted (2026-08-28)

Afactory started as a deterministic Review Kernel. Its domain model is deliberately specific:
Subjects, Campaigns, Rounds, Reports, Findings, Demands, and a Ledger express review guarantees
that a generic task graph cannot express as precisely. The next product slices must also accept a
pull request, ticket, or design idea and produce durable outputs without forcing those inputs into
review-specific vocabulary.

Add a product orchestration layer above the Review Kernel. The product model has five concepts an
operator must understand:

- **Reference** locates an external mutable object, such as a GitHub pull request, tracker ticket,
  URL, file, or submitted idea. Resolving a Reference always captures a new immutable Artifact;
  the Reference itself is never execution authority or content identity.
- **Artifact** is immutable, typed, content-addressed data with producer and exact input
  provenance. Input and output are roles an Artifact occupies at a Workflow boundary, not
  separate entities.
- **Workflow** is a versioned recipe with typed input and output ports. A Run pins the exact
  Workflow version and resolved definition it executes.
- **Run** is one durable execution of a Workflow with exact input Artifacts, a resolved Profile,
  policy, state transitions, Attempts, outputs, and Receipts. It may wait, resume, cancel, or fail
  without losing the execution boundary it already established. Execution status is distinct from
  the Workflow's domain result: a completed review with blocking Findings is a successful Run
  containing a failing review verdict, not an execution failure.
- **Profile** is operator-facing configuration for Agents, checks, models, policies, budgets, and
  publication posture. A mutable profile selection is compiled into immutable resolved authority
  before execution.

The internal execution model adds these concepts:

- A **Capability** is a versioned typed operation, such as `review.pull-request@1`,
  `design.specification@1`, `implement.plan@1`, or `verify.change@1`.
- A **Step** invokes one Capability with explicit input bindings inside a Workflow.
- An **Agent** is versioned behavior: instructions, allowed tools, and model requirements.
- An **Attempt** is one fenced execution of a Step. The existing Review Kernel Attempt keeps its
  exact semantics inside review; the product record links to it rather than rewriting it.
- A **Gate** records an automatic or human decision required for progress.
- A **Receipt** is an Artifact proving an external effect, such as a published review or created
  pull request. The effect and Receipt are produced by an explicitly named Step.
- A **Worker** is an ephemeral runtime process that executes an Attempt. It is not a durable
  product object or the primary unit of user configuration.

Artifact types are nominal and versioned. Ports require exact type, cardinality, and optionality;
adapters are explicit Capabilities rather than implicit conversions. Initial namespaces include
`afactory/*`, `scm/*`, `tracker/*`, `design/*`, `code/*`, `verification/*`, `review/*`, and
`delivery/*`. Every type has a schema. Secrets, reusable provider credentials, and ambient login
state are capabilities used by execution policy and never Artifacts.

The Review Kernel remains a specialized capability with its existing contracts and language:

```text
Afactory Run
  -> review.pull-request@1 Step
       -> Review Campaign
            -> Rounds
                 -> Reports -> Findings -> Convergence
```

`Subject`, `Campaign`, `Round`, `Report`, `Finding`, `Demand`, and `Ledger` remain Review Kernel
terms. The existing typed review DAG remains the kernel's internal representation. A review
Profile may compile into that DAG, but a product Workflow does not expose review-only concepts
such as `snapshot_affinity`, gather nodes, or Ledger nodes as its universal authoring model.

Four initial product-policy decisions are binding:

1. GitHub is the first source-control integration; GitLab follows a proven GitHub slice.
2. Review analysis does not publish remotely by default. Publication is a separate explicit Step
   that returns a Receipt.
3. Pull-request review authority comes from the trusted base branch plus owner-controlled policy;
   pull-request head content cannot change the rules used to review itself.
4. Ticket implementation requires a human Gate between the accepted Plan and implementation.

## Considered options

- **Generalize the Review Kernel graph into the product model.** Rejected because reviewer,
  gather, Ledger, convergence, and snapshot-affinity rules are review-specific. Making every
  Workflow pretend to be a review would leak internal machinery into ordinary configuration and
  weaken the kernel's language.
- **Replace the Review Kernel with a generic durable workflow engine.** Rejected because the
  kernel's append-only evidence, identity, closure, and convergence rules are working product
  assets, not accidental implementation details.
- **Build three independent products for review, implementation, and design.** Rejected because
  they need the same immutable Artifact, provenance, Profile, Run, Gate, Attempt, budget, and
  effect-receipt boundaries.
- **Add a product orchestration layer and keep specialized capabilities (chosen).** This gives
  operators one small model while preserving stronger domain models behind each Capability.

## Consequences

- [`../product-roadmap.md`](../product-roadmap.md) is the product roadmap. The M0-M9 backlog in
  [`../backlog.md`](../backlog.md) remains the dependency-ordered Review Kernel backlog and no
  longer determines product sequencing by itself.
- New product contracts are additive. Existing `.review/`, `review.kernel/*`, event, CAS,
  Campaign, and Ledger contracts are not renamed or reinterpreted.
- Product Artifact types cross into the Review Kernel only through an explicit checked adapter
  Capability that produces the existing `review.kernel/*` inputs. No product type silently
  aliases a frozen kernel type.
- Every Run pins resolved Workflow and Profile authority plus exact input Artifact IDs before a
  Step executes. Resume never re-resolves mutable References implicitly.
- External effects are absent from analysis and generation Steps. An effect requires a separate
  named Step, explicit policy authorization, idempotency, and a non-secret Receipt.
- The first vertical slice is GitHub pull-request review. Ticket-to-pull-request and design
  workflows reuse the proven product layer and invoke review rather than duplicating it.
- Generic Workflow authoring remains closed until the three product workflows prove which
  abstractions are stable.
