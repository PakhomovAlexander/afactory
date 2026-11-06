# ADR-2026-11-06-bind-executable-review-evidence-to-evaluation: Bind executable review evidence to evaluation

**Status:** proposed (2026-11-06).

## Context

Implementation reviewers can execute checks in a private writable clone while their declared
source remains immutable. Their flat Reviewer Result records findings, not build, private
installation and test results. Ordinary implementation packages need to pass those results to
an independent evaluator without embedding project orchestration or Provider policy in AF.

## Considered options

- Add execution fields to Reviewer Result. Rejected: this changes a distinct established contract.
- Infer testing from reviewer prose, role names or a pipeline name. Rejected: none is typed,
  Snapshot-bound execution evidence, and packages own their own Worker team.
- Add a separate typed receipt and explicit evaluator edge. Selected: ordinary package contracts
  choose the capability and preserve the exact evidence identity.

## Decision

Add `af/ImplementationTestEvidence@1` and `af/ImplementationEvaluationInput@1`.
The evidence payload names its exact Snapshot, status, reason and bounded commands with purpose,
exit code (null when unavailable) and public evidence. A passed receipt requires successful build,
private install and test commands. Failed and unavailable receipts are valid negative evidence.

A declaring reviewer has `execute-checks`, no `write-source`, one required current-Snapshot evidence
output, and retains its source and checks. The flat Reviewer Result is unchanged. Native reviewer
admission validates evidence independently and refuses stale payload/envelope identities. Existing
source immutability enforcement still discards scratch and rejects declared-source changes.

An opting-in evaluator declares one required `testing` input of that evidence type with
`same_as source` affinity, and retains it in its evaluation output. The compiler refuses missing,
optional, unbound or unretained evidence contracts. Native evaluation admission checks the exact
input artifact, envelope and payload against the evaluator source. A pass requires passed evidence;
missing, failed, unavailable or stale evidence cannot support it. Final implementation acceptance
rechecks retained testing evidence. Ordinary pipelines declaring neither contract retain their
current behavior. No persisted existing shape changes and no compatibility readers are added.

Packages choose reviewer/evaluator slots, independence, instructions, model and Provider bindings
using existing contracts. AF adds no pipeline, role, model or account naming policy. This evidence
is a reviewer claim about execution, not kernel-observed Measurement or CI, publication or merge
permission. The local gate remains independent. No separate verification Task is required.

## Consequences

Consumers review and repin their changed packages. They must route the same sealed candidate to
reviewer and evaluator and expose the review evidence through any child pipeline boundary.
Commands and public output must omit secrets and host-specific paths. Unavailable tools require
honest negative evidence rather than a claimed pass. No Scenario, Ant, lifecycle, Git publication,
workspace cleanup, warm-cache or performance-model behavior changes.

See [usage and fixture](../task-execution/executable-review.md).
