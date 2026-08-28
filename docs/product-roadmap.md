# Afactory product roadmap

**Status:** accepted product direction (2026-08-28)

## Goal

Make Afactory useful through three human-facing workflows, in this order:

1. submit any GitHub pull request and receive a high-quality review under trusted configuration;
2. submit a tracker ticket and receive a verified draft pull request that is reviewed by the same
   review capability;
3. submit an idea and receive reviewable design artifacts ending in an implementation plan.

The operator works with References, Artifacts, Workflows, Runs, and Profiles. The durable model and
the boundary around the existing Review Kernel are fixed by
[ADR-0035](adr/0035-separate-product-orchestration-from-review-kernel.md).

## Product contract

```text
Reference
  -> immutable input Artifact(s)
  -> versioned Workflow + resolved Profile
  -> durable Run
  -> immutable output Artifact(s)
  -> optional explicit effect Step -> Receipt
```

The default user surface stays task-oriented:

```sh
af review https://github.com/org/repo/pull/123 --profile strict
af run show RUN_ID
af review publish RUN_ID

af implement JIRA-123 --profile backend
af design idea.md --profile architecture
```

Generic graph authoring, internal node kinds, artifact wiring, and Review Kernel
`snapshot_affinity` do not appear in these commands or ordinary Profile configuration.

## Product principles

- **Immutable before executable.** Resolve every mutable Reference into an immutable Artifact
  before any model or Gate consumes it.
- **Exact authority.** A Run pins Workflow, resolved Profile, policy, and all input Artifact IDs.
  Resume reuses them unless the operator explicitly starts a new revision or Run.
- **Typed composition.** Ports use exact nominal type versions, cardinality, and optionality.
  Every conversion is a named Capability with its own output provenance.
- **Visible effects.** Analysis and generation are local by default. Publish, push, comment, and
  create-pull-request operations are separate idempotent Steps with Receipts.
- **Specialized kernels.** Product orchestration composes capabilities; it does not dilute the
  Review Kernel's Campaign, evidence, and convergence model.
- **Minimum context and bounded spend.** Every Attempt receives an exact context manifest and a
  budget tied to a named informational purpose.
- **Product slices before framework surface.** Prove the same model through review,
  implementation, and design before exposing a generic SDK or Workflow DSL.

## Sequence

### P0 - Product contracts and executable skeleton

Define the smallest additive product layer without changing persisted Review Kernel contracts.

Deliver:

- nominal, versioned schemas for product Run, resolved Profile, Step, Gate, and Receipt records;
- registries for Artifact types and Capability input/output contracts;
- an append-only Run state machine with `planned`, `running`, `waiting`, `succeeded`, `failed`,
  and `cancelled` terminal semantics;
- a separate typed Workflow result, so a completed review with a failing verdict is a successful
  execution with blocking Findings rather than a failed Run;
- exact Workflow/Profile/input pinning before dispatch;
- a product Run linked to one existing review Campaign, with replay and CLI inspection;
- CLI skeletons for `af run show`, `af run list`, and machine-readable output;
- compatibility tests proving no `.review/`, `review.kernel/*`, Campaign, Ledger, or existing CAS
  identity changes.

Exit when one local review can be represented and replayed as a product Run whose authoritative
result still comes from the unchanged Review Kernel.

### P1 - GitHub pull-request review

Make pull-request review the first complete product workflow.

```text
GitHubPullRequest Reference
  -> scm/PullRequestRevision@1
  -> source Snapshot + code/ChangeSet@1 + scm/PullRequestContext@1
  -> review.pull-request@1
  -> review/ReviewResult@1
```

Deliver:

- strict parsing and normalization of public and private GitHub pull-request References;
- authenticated metadata and Git object capture into one immutable pull-request revision;
- trusted authority resolution from the base branch plus owner policy before candidate capture;
- an explicit checked adapter from product source Artifacts to the existing `review.kernel/*`
  Subject, Snapshot, and Change Set contracts;
- a simple review Profile that compiles to the existing locked typed review pipeline;
- `af review <github-url> --profile <name>` with no manual worktree, authority ref, or temporary
  pipeline setup;
- `ReviewResult` containing summary, verdict, Findings, checks, evidence links, spend, and exact
  source revision;
- stable text and JSON inspection through the product Run;
- restart behavior that creates a new pull-request revision when the remote head changes rather
  than mutating the prior input.

Exit when a clean machine can review a real pull request using only its URL and trusted Profile,
then reproduce exactly which revision, authority, configuration, checks, and model Attempts
produced the result.

### P2 - Controlled publication and durable operation

Turn the local result into a safe operational workflow without making remote mutation implicit.

Deliver:

- `af review publish RUN_ID` as a separate explicit, idempotent Step;
- GitHub check/comment/review rendering derived only from an exact `ReviewResult` Artifact;
- `delivery/GitHubReviewReceipt@1` with repository, pull request, source revision, result Artifact,
  remote object identifiers, and idempotency key, but no credential material;
- retry, resume, cancellation, waiting, timeout, provider admission, and budget inspection at the
  product Run level;
- human Gates represented as durable waits rather than a blocked Worker process;
- an operator-visible audit trail for every effect attempt and its outcome.

Exit when repeating publication cannot duplicate a review, a crash can resume without repeating
an acknowledged remote effect, and no analysis command can publish by accident.

### P3 - Review quality bake-off

Prove product quality before expanding the workflow surface.

Deliver:

- a versioned evaluation corpus of representative real pull requests plus seeded defects;
- baseline and candidate Profiles evaluated on the same immutable pull-request revisions;
- measurements for accepted Findings, false positives, missed seeded defects, duplicate claims,
  evidence quality, latency, token spend, and human editing before publication;
- failure classification separating capture, Gate, provider, reviewer, gather, and publication
  failures;
- release thresholds and a repeatable report that can reject a worse Profile or model change.

Exit when the default review Profile meets written quality and cost thresholds on repeated runs,
not merely when the workflow completes.

### P4 - Ticket to verified draft pull request

Compose implementation from product capabilities and reuse the review workflow.

```text
TrackerIssue Reference
  -> tracker/Issue@1
  -> design/Specification@1
  -> design/ImplementationPlan@1
  -> human Plan Gate
  -> code/ChangeSet@1
  -> verification/TestReport@1
  -> explicit create-draft-pull-request Step
  -> delivery/PullRequestReceipt@1
  -> review.pull-request@1
  -> review/ReviewResult@1
```

Deliver:

- GitHub Issue first, followed by a Jira adapter using the same `tracker/Issue@1` boundary;
- specification and implementation-plan Artifacts with explicit assumptions and acceptance
  criteria;
- a mandatory human approval Gate between Plan and implementation;
- implementation in a sealed workspace with exact context, budgets, and verification inputs;
- explicit branch, push, and draft-pull-request effects with separate Receipts;
- invocation of the existing pull-request review capability on the created revision;
- recovery that never silently re-plans, re-pushes, or opens a second pull request.

Exit when one accepted ticket can produce a verified draft pull request and ReviewResult while a
rejected or unanswered Plan Gate causes no code or remote effect.

### P5 - Idea to design package

Prove that the product model works without a repository or pull request as its primary subject.

```text
design/Idea@1
  -> design/Brief@1
  -> design/Options@1
  -> design/Decision@1
  -> design/ImplementationPlan@1
```

Deliver:

- file, stdin, and URL capture into an immutable `design/Idea@1`;
- explicit problem framing, constraints, unknowns, and success criteria in the Brief;
- multiple comparable Options with evidence, trade-offs, and rejected alternatives;
- a human decision Gate before an ADR and implementation plan become accepted outputs;
- Markdown export as an explicit projection of canonical Artifacts;
- Profiles that select research depth, reviewers, evidence policy, budget, and output templates.

Exit when a design Run can be resumed, challenged, and regenerated from exact inputs without
requiring code capture or pretending its output is a Review Kernel Finding Ledger.

### P6 - Generic authoring and additional integrations

Open extension points only after the three product workflows establish stable abstractions.

Deliver:

- a public Workflow schema and validation tooling;
- a Capability SDK with explicit purity/effect declarations and typed contracts;
- custom Artifact type registration with schema compatibility checks;
- GitLab and additional tracker/source adapters;
- reusable UI and API surfaces over the same Run state and Receipts.

Exit when a third party can add a Capability without access to Afactory internals, weaken no
authority boundary, and compose it with the three built-in workflows through exact types.

## Relationship to the Review Kernel backlog

[`backlog.md`](backlog.md) remains authoritative for Review Kernel capability dependencies. It is
not the product release sequence. Product phases pull kernel work only when an exit criterion
requires it:

- P0-P1 use the existing Campaign, typed graph, immutable evidence, provider, and convergence
  boundaries; operator JSON and spend views pull the required M5 slices forward.
- P2 pulls the brokered external-capability and revocation boundary from M6 before GitHub
  publication is allowed.
- P4 pulls Proposal, verification, and delivery work needed for a verified implementation, but
  does not wait for unrelated dynamic scatter or automatic integration work.
- M8 dynamic scatter and M9 automatic internal Integration remain kernel improvements until a
  measured product bottleneck or product exit criterion requires them.

Kernel milestones still require `make check` and their accepted review policy. Product progress
cannot mark an incomplete kernel invariant complete, and kernel completeness alone cannot claim a
product phase has passed.

## First implementation train

The first train is deliberately linear until the persisted product contracts are accepted. Each
item is one reviewable change with a green repository gate:

1. **P0.1 - Contract vocabulary.** Add schemas and Rust views for product Artifact type IDs,
   Capability contracts, resolved Profiles, Workflow manifests, product Runs, Steps, Gates, and
   Receipts. Pin positive, adversarial, and compatibility fixtures before adding dispatch.
2. **P0.2 - Product Run store.** Persist and replay the Run state machine, exact input bindings,
   resolved Workflow/Profile digests, Step Attempts, waits, outputs, and Receipt references.
   Refuse illegal transitions and missing Artifact authority.
3. **P0.3 - Review bridge.** Start one existing local review Campaign from a product Step, persist
   the Campaign ID as nested capability authority, and project its terminal result without
   changing Campaign replay or Ledger identity.
4. **P0.4 - Operator inspection.** Add `af run list/show --format text|json`; distinguish
   execution status, waiting reason, and domain result. Prove replay and recovery in CLI tests.
5. **P1.1 - GitHub Reference capture.** Resolve a pull-request URL into an immutable
   `scm/PullRequestRevision@1` plus exact source and context Artifacts. No model dispatch and no
   remote mutation in this slice.
6. **P1.2 - Trusted review authority.** Resolve Profile and repository policy from the base branch
   plus owner policy before candidate capture; add adversarial tests proving head content cannot
   replace either source of authority.
7. **P1.3 - Product-to-kernel adapter.** Validate and convert the captured product Artifacts into
   the existing Review Kernel Subject inputs through one typed Capability. Keep all frozen kernel
   types and identities byte-compatible.
8. **P1.4 - URL-to-result vertical slice.** Wire `af review <url> --profile <name>` through a
   durable Run and emit `review/ReviewResult@1` in text and JSON. Dogfood it on pinned real pull
   requests before adding publication.
9. **P2.1 - Explicit publication.** Add the brokered GitHub effect Step, idempotency record, and
   `delivery/GitHubReviewReceipt@1`; prove crash recovery and duplicate suppression against a test
   remote before enabling a real repository pilot.

Do not start ticket implementation, design authoring, generic Workflow syntax, or a broad UI on
this train. Their requirements may inform schema review, but they do not expand P0-P2 scope.

## Release gates

Every product phase must provide:

- additive schemas with positive and adversarial parity tests;
- deterministic replay of accepted state and fail-closed handling of missing authority;
- exact context and spend accounting for every model Attempt;
- a threat-model update for any new Reference resolver or external effect;
- CLI text and JSON acceptance tests;
- a real end-to-end pilot on pinned inputs;
- a fresh external correctness review under the repository's accepted review policy;
- a recovery drill at every durable wait or effect boundary introduced by the phase.

P1 and later releases additionally record quality metrics from P3's harness once it exists.

## Explicitly deferred

- GitLab before the GitHub workflow is proven.
- Automatic publication as part of analysis.
- Candidate-controlled review policy from a pull-request head.
- Implementation before the human Plan Gate approves it.
- Generic DAG, SDK, or plugin authoring before review, implementation, and design all use the
  product model.
- A hosted control plane, distributed scheduler, marketplace, or broad UI before local durable
  Runs and effect receipts are reliable.
- Renaming frozen Review Kernel contracts merely to match product-level terminology.
