# Task execution: proposed configuration examples

**Status:** illustrative proposal. These snippets parse as TOML but are **not
accepted by Afactory 0.8.0**. Exact schema and compiler behavior belong to the T0 contract work
in the [increment design](task-execution.md). Example artifact names describe proposed ports;
they do not rename any existing persisted artifact type.

## 1. One Task, independent of its strategy

An imported issue or local file produces this same contract in the Store. Files can supply
input or reusable fixtures; live Task state never becomes a Git coordination mechanism.

```toml
schema = "af.task.proposed/1"
kind = "implement"
goal = "Add a pagination limit of 1 to 100 to the list endpoint"
strategy = "fast"
fallback = "refuse"

[inputs]
source = { artifact = "input:source-snapshot", type = "snapshot" }
requirements = { artifact = "input:normalized-requirements", type = "task-requirements" }

[facts]
scope = "single-module"
database_change = false

[acceptance]
required = ["unit", "api-compatibility", "independent-acceptance"]
result_type = "snapshot"

[limits]
tokens = 120000
wall_seconds = 600
verification_reserve_tokens = 30000
```

The input references must resolve to exact artifacts with these recorded types before selection.
The Jira adapter normalizes the imported issue into `task-requirements` and records the raw issue
artifact/revision as provenance; the ID itself is not a schema or an implicit conversion.
Root-adapter defaults are limited to the declared constructors in design section 3.
`fallback` applies when an explicit Pipeline is requested: `refuse` rejects an invalid choice;
`select` searches the catalog; `generate` searches first and requests generation only on semantic
no-match, within trusted policy. Automatic selection uses the project's `no_match` policy.
The facts shown
are trusted/bounded scope declarations for this example, not a model's unchecked assurances.
If scope cannot be established, this Task does not match the small Pipeline.

## 2. A shared Pipeline and its named Worker slots

This example shows the successful data path. The `verify` operator always returns a typed
verification result: failed mandatory checks produce unsatisfied acceptance without invoking
the evaluator. A check that could not run yields inconclusive acceptance. Runtime failures
and exhausted Attempts remain explicit outcomes.

```toml
schema = "af.pipeline.proposed/1"
name = "team/implement-small"
version = "1.0.0"

[accepts]
kinds = ["implement"]
required_facts = { scope = "single-module", database_change = false }
supported_obligations = ["unit", "api-compatibility", "independent-acceptance"]

[contract.inputs]
source = "snapshot"
requirements = "task-requirements"

[contract.outputs]
result = "snapshot"
verification = { type = "verification-result", covers = ["unit", "api-compatibility", "independent-acceptance"] }

[strategy]
class = "fast"
max_attempts = 3
max_parallel = 1

[slots.implementer]
default = "team/implementer"
requires = ["implement-input", "sealed-worker-output"]
allow_local_replacement = true
max_attempts = 2

[slots.evaluator]
default = "team/acceptance"
requires = ["evaluation-input", "evaluation-verdict"]
independent_from = ["implementer"]
allow_local_replacement = true
min_attempts = 1
max_attempts = 1

[[nodes]]
id = "implement"
op = "worker"
slot = "implementer"
inputs = { source = "input.source", requirements = "input.requirements" }

[[nodes]]
id = "seal"
op = "seal"
inputs = { source = "input.source", output = "implement.output" }

[[nodes]]
id = "checks"
op = "check"
checks = ["policy:unit", "policy:api-compatibility"]
inputs = { snapshot = "seal.snapshot" }

[[nodes]]
id = "accept"
op = "verify"
slot = "evaluator"
obligation = "independent-acceptance"
inputs = { source = "input.source", snapshot = "seal.snapshot", changes = "seal.changes", requirements = "input.requirements", gates = "checks.results" }

[outputs]
result = "seal.snapshot"
verification = "accept.result"

[coverage]
unit = "checks.results"
api-compatibility = "checks.results"
independent-acceptance = "accept.result"
```

Every Pipeline declares `[contract.inputs]` and `[contract.outputs]`; `[outputs]` binds
those public outputs to internal producers. `input.*` refers to the public interface,
populated by a root Task adapter or an enclosing Pipeline call. The short type names here
resolve to exact versioned schemas in the locked catalog, with required single-value ports;
optional/conditional ports must explicitly declare their cardinality and terminal handling.
Port affinity constraints are checked against the locked schemas and operator signatures.
`policy:unit` resolves to trusted check authority; a generated Pipeline cannot
replace that authority with `echo passed`. Coverage references identify verifier receipts,
not just matching strings in model output. The compiler expands and validates them.
Public `covers` lists obligations the output's retained evidence can establish. The internal
`[coverage]` mapping must prove each such path; callers depend on public coverage, not knowledge
of child node names. The `verify` result retains the referenced check and evaluator receipts.

Any implementation Pipeline may invoke a Review subpipeline between seal and acceptance;
section 8 makes its contract explicit. A heavy Pipeline may also decompose work through
bounded Scatter or reserve a repair attempt. It must wire all resulting
evidence into final verification. The total `max_attempts` includes every model call and retry.
One evaluator Attempt is protected at admission, so the implementer may consume at most two
of the three. A transport retry cannot borrow the evaluator's reserved count or token/time
capacity. Embedded reviewers and repair verifiers require their own admitted reservations.

## 3. One Worker used by several Pipelines

```toml
schema = "af.worker.proposed/1"
name = "team/implementer"
version = "2.1.0"
role = "implementer"
instructions = "worker.md"
input = "implement-input"
output = "sealed-worker-output"
tools = ["repository-read", "sandbox-edit", "project-tests"]
minimum_environment = "process"

[defaults]
provider = "coding"
effort = "medium"

[tuning]
allowed = ["provider", "model", "effort"]

[tests]
fixtures = ["creates-required-file", "preserves-protected-files", "honors-output-contract"]
```

The tool and environment names resolve through the trusted catalog and machine bindings.
Contract tests include command-based protocol fixtures and separately budgeted behavioral
evaluations; parsing this file does not prove that every model obeys the instructions.

## 4. Alice and Bob tune bindings without copying the Pipeline

```toml
# Alice's machine-local config; model names are operator-defined aliases.
[bindings."team/implement-small".implementer]
worker = "local/alice-implementer"
provider = "coding-local"
model = "fast-code"
effort = "medium"

[bindings."team/implement-small".evaluator]
worker = "team/acceptance"
provider = "independent-review"
model = "careful-review"
effort = "high"
```

```toml
# Bob's machine-local config.
[bindings."team/implement-small".implementer]
worker = "team/implementer"
provider = "coding-alt"
model = "careful-code"
effort = "high"
```

Both execute the same Pipeline definition; their Execution Plans have different Worker or
effective-binding digests. Credentials stay in each Provider's local authentication context.
Alice's custom instructions form a separate captured Worker package. To share it, export the
portable package and fixtures, commit them, and bind its new shared name.
The independent slots use different Worker digests and isolated sessions. Their Provider bindings
must resolve to distinct authenticated principals under default policy; aliases of one account
do not qualify. Same-principal tuning requires an explicit trusted project-policy exception,
and still cannot share the implementer's Worker digest or private session/context.

## 5. Project policy controls selection and generation

```toml
[task_selection]
prefer = ["team/implement-small", "team/implement-heavy", "builtin/implement-heavy"]
ambiguity = "refuse"
no_match = "generate"

[planning]
worker = "team/planner"
execution = "require-developer-approval"
max_attempts = 2
max_expanded_nodes = 64
max_subpipeline_depth = 4
max_parallel_shards = 16
tokens = 12000
allow_new_executables = false

[verification]
required = ["unit", "api-compatibility", "independent-acceptance"]
allow_local_removal = false
```

Project limits and Task limits intersect. The twelve-thousand-token planning cap is part
of the Task's total, not additional credit. If a compatible Pipeline exists, the Planner
receives zero calls. Each Task kind may have different verification obligations; the
example policy is scoped to implementation Tasks, not a demand to run code tests on every
document Task.
The Planner may produce a proposal within these limits, but the generated execution graph
waits for developer review and exact-plan approval. This requirement cannot be disabled by
local tuning or generated TOML. A changed Task revision, dependency, binding, permission,
budget, or input requires a new approval; resuming the same approved plan retains its receipt.

## 6. Share a catalog through Git

```toml
[[catalog.imports]]
namespace = "team"
git = "ssh://git.example.org/team/af-workflows.git"
revision = "release-1.0"
path = "catalog"
```

`catalog sync` resolves the selector during an explicit dependency update and writes the
exact Git commit plus content digests into `.af/af.lock`. Runtime reads only that locked
closure, never the moving selector. The lock is machine-generated; these examples deliberately
do not invent executable checksums.

Imported names require explicit aliases when a project wants to override them. Local
packages remain distinct from `team/*`; no directory-search order silently changes meaning.

## 7. Required conformance examples

| Input/change | Expected result |
|---|---|
| Small Task fits the shared definition | Selected with zero planning model calls |
| Required fact unknown or database migration required | Small Pipeline rejected with its missing predicate |
| Explicit incompatible Pipeline, default fallback | Refusal with exact reasons |
| Explicit incompatible small Pipeline, `fallback = "generate"`, compatible heavy Pipeline exists | Select heavy with zero Planner calls |
| Explicit incompatible Pipeline, no compatible definition | `select` returns no-match; `generate` requests policy-admitted planning |
| Two unordered compatible definitions | Ambiguous selection, both names shown |
| Preferred compatible definition exceeds budget, another fits | Select feasible alternate and explain the rejection |
| Verifier minimum allocation exceeds Task budget | Refuse before implementation dispatch |
| Implementation uses its unprotected allocation | Further implementation refused; protected verifier allocation remains reserved |
| Implementer transport failure then successful retry under the three-Attempt example | Two implementer Attempts charged; evaluator's reserved third Attempt remains dispatchable |
| Provider exceeds its reservation | Full spend charged, budget breach recorded, no dispatch beyond remaining authority |
| Generated Pipeline omits API compatibility | Compiler rejection before execution |
| Valid generated plan, no developer approval | Persist proposal and wait for plan review; dispatch zero generated-graph nodes |
| Developer approves the exact generated plan | Recheck authority, budget and deadline; execution may start |
| Generated child inside a shared Pipeline | Review the expanded parent plan before any of its execution nodes dispatch |
| Worker, input, dependency, permission or budget changes after approval | New plan digest; fresh developer approval required |
| Developer rejects a plan, or its waiting deadline expires | No execution; record rejection or exhaustion |
| Planner tries to approve its own proposal | Refuse; model identity cannot authorize execution |
| Crash while awaiting plan review | Resume the same pending proposal without regeneration or automatic approval |
| Crash after approval, before execution | Reuse approval only for the same immutable plan; recheck admission |
| Another Task uses a previously generated proposal's approval | Refuse Task-revision mismatch |
| Two role-compatible slots bind the same multi-role Worker digest | Reject independence after successful type checks |
| Distinct Workers use two aliases of the same Provider principal under default policy | Reject principal independence; alias names do not establish separation |
| Unit check fails on derived Snapshot | Unsatisfied acceptance; evaluator not dispatched |
| Check cannot execute | Inconclusive acceptance; no verified result |
| Required reviewer times out | Execution incomplete; legacy review exit 4 |
| Complete light review produces blocking findings | Review domain requests changes; legacy review exit 3 even if producing a review satisfies the Task goal |
| External effect uncertain after crash | Wait with `needs-human`; do not repeat the effect |
| Worker changes after Plan admission | Existing Plan resumes captured bytes; new Task gets new binding |
| Crash after Planner output, before final Plan admission | Resume bootstrap receipts and captured proposal without another Planner call |
| First Snapshot fails, one declared semantic repair succeeds | Expanded repair nodes verify the second Snapshot; first evidence cannot satisfy it |
| Generated Pipeline exported, tested, imported | Second matching Task reuses it with zero Planner calls |
| New Task kind produces a document | Same runtime, typed document result and appropriate acceptance |

These are future implementation tests. Current validation of this design checks TOML parsing
and documentation links only; it does not establish compiler conformance.

## 8. Embed the same Review Pipeline used by a review Task

This is the public-interface sketch of `team/review`; its internal check, reviewer, and
reducer nodes are omitted here. `accepts.kinds` controls root Task selection. Embedded
callers satisfy its typed ports regardless of the parent Task kind. This is the same mandatory
interface as the implementation Pipeline in section 2. A complete Review definition also binds
each declared output to an internal producer through `[outputs]`.

```toml
schema = "af.pipeline.proposed/1"
name = "team/review"
version = "1.0.0"

[accepts]
kinds = ["review"]

[contract.inputs]
subject = "review-subject"
requirements = "task-requirements"
prior_findings = "finding-set"
prior_demands = "demand-set"

[slots.correctness]
default = "team/correctness"
requires = ["reviewer-input", "reviewer-result"]
allow_local_replacement = true

[contract.outputs]
checks = { type = "check-result-set", covers = ["unit", "api-compatibility"] }
findings = "finding-set"
demands = "demand-set"
assessment = { type = "review-assessment", covers = ["review-completeness"] }
```

The following is a fragment of `team/implement-reviewed`, inserted after its `implement`
and `seal` nodes. The complete parent declares public `source`, `requirements`, and
`review_history` inputs and `result`/`verification` outputs, binding outputs as in section 2.
Its declared root default may supply typed empty `input.review_history` only for a new lineage.
`review-bind` derives the Subject and exact input Sets for the current candidate; the source
Snapshot remains the comparison Base.

```toml
[contract.inputs]
source = "snapshot"
requirements = "task-requirements"
review_history = { type = "review-history", root_default = "empty-review-history" }

[contract.outputs]
result = "snapshot"
verification = { type = "verification-result", covers = ["unit", "api-compatibility", "review-completeness", "independent-acceptance"] }

[slots.reviewer]
default = "team/correctness"
requires = ["reviewer-input", "reviewer-result"]
independent_from = ["implementer"]
allow_local_replacement = true

[[nodes]]
id = "review_inputs"
op = "review-bind"
inputs = { base = "input.source", head = "seal.snapshot", history = "input.review_history" }

[[nodes]]
id = "review"
op = "call"
pipeline = "team/review"
bindings = { correctness = "slot.reviewer" }
inputs = { subject = "review_inputs.subject", requirements = "input.requirements", prior_findings = "review_inputs.findings", prior_demands = "review_inputs.demands" }

[[nodes]]
id = "accept"
op = "verify"
slot = "evaluator"
inputs = { source = "input.source", snapshot = "seal.snapshot", requirements = "input.requirements", review = "review.assessment", gates = "review.checks", findings = "review.findings", demands = "review.demands" }

[coverage]
unit = "review.checks"
api-compatibility = "review.checks"
review-completeness = "review.assessment"
independent-acceptance = "accept.result"
```

The parent has no duplicate unit/API check node: this Review Pipeline owns those Gates.
Its `verify` node requires complete review, acceptable domain conclusion, satisfied
obligations, and final independent acceptance. An empty Finding Set without complete review
does not satisfy it. A declared repair branch consumes findings, produces S2, and obtains
current-S2 evidence as described in the design. Review mode and round count remain pinned;
a heavy strategy's second review invocation continues its lineage rather than opening
another light Campaign.

For a light strategy with one repair, the expanded branch below follows the repair Worker and
`repair_seal` (S1-to-S2). It uses a separate independent `fix_verifier` slot with required
`fix-verification-input`/`fix-verification-set` contracts and protected Attempt capacity.
The original `review.findings` and `review.demands` remain the claims to address. This fragment
shows the repair-only branch; the compiler also expands the bounded branch/select control and
exhaustive failure paths. `repair.output.fix_regions` is the repair Worker's proposed mapping
of Finding IDs to changed regions, validated against the sealed Change Set.

```toml
[[nodes]]
id = "repair_subject"
op = "review-bind"
inputs = { base = "input.source", head = "repair_seal.snapshot", prior_findings = "review.findings", prior_demands = "review.demands" }

[[nodes]]
id = "repair_checks"
op = "check"
checks = ["policy:unit", "policy:api-compatibility"]
inputs = { snapshot = "repair_seal.snapshot" }

[[nodes]]
id = "repair_attest"
op = "attest-fixes"
inputs = { subject = "repair_subject.subject", findings = "repair_subject.findings", changes = "repair_seal.changes", regions = "repair.output.fix_regions" }

[[nodes]]
id = "repair_verify"
op = "fix-verify"
slot = "fix_verifier"
inputs = { subject = "repair_subject.subject", prior_findings = "repair_subject.findings", prior_demands = "repair_subject.demands", attestations = "repair_attest.attestations", gates = "repair_checks.results" }

[[nodes]]
id = "repair_accept"
op = "verify"
slot = "evaluator"
inputs = { source = "input.source", snapshot = "repair_seal.snapshot", requirements = "input.requirements", repair = "repair_verify.assessment", fix_receipts = "repair_verify.receipts", gates = "repair_checks.results", findings = "repair_verify.findings", demands = "repair_verify.demands" }
```

`repair_verify` produces per-Finding S2 receipts and a targeted repair assessment, not a new
whole-S2 review. Its assessment retains the S1 review evidence, exact repair delta, and new
verification scope. A parent advertising `review-completeness` on every final Snapshot cannot
route that output from this branch: it must use a heavy strategy that admits a new complete
review, or return unsatisfied/inconclusive acceptance with that obligation open. A light-repair
variant explicitly advertises `review-and-targeted-repair`, with trusted policy defining the
allowed repair scope and requiring current Gates and independent final evaluation. This is a
different declared coverage guarantee, never an implicit weakening of the Task's obligations.

```text
Shared definitions                    One compiled parent execution
------------------                    -----------------------------
implement-reviewed.toml               implement
  call team/review --------+          seal
                          |          review_inputs
review.toml <-------------+          review.checks
  checks                             review.correctness
  correctness                        review.reduce
  reduce                             accept

One Task ID, one budget with child caps, one recorded execution hierarchy.
```

Required composition conformance cases:

- Reject any root, embedded, or generated Pipeline missing its public input/output contract,
  a required output binding, or a compatible schema/cardinality/affinity declaration.
- Invoke an implementation or document Pipeline from a parent with a different Task kind;
  it reads only its bound business inputs and retains the parent's execution context.
- Fail a child before it produces a required output; downstream consumers receive an explicit
  unavailable-output outcome, never a fabricated empty value or successful completion.
- Run the same locked Review definition standalone and embedded with equivalent declared
  inputs/bindings; normalized review artifacts agree under deterministic test Workers.
- Pass S0-to-S1 to the embedded call while live HEAD changes; it still reviews S0-to-S1.
- Report incomplete review with zero findings; parent acceptance remains inconclusive.
- Return a blocking Finding; the implementation Task cannot be verified.
- Remove API compatibility from the child's public `checks.covers`; the parent's coverage
  fails compilation. Remove its internal check but retain the claim; child compilation fails.
- Bind the implementer and reviewer to one multi-role package that satisfies both port
  contracts; independence admission rejects the equal digest even though type checks pass.
- Exhaust the parent budget inside review; the child cannot start an independent allowance.
- Repair S1 into S2; only `repair_verify` receipts bound to the current Finding views,
  Attestations and S0-to-S2 Subject may close the obligations. Stale S1 receipts cannot.
- Require whole-S2 review after a light repair; targeted fix verification leaves that coverage
  obligation open. It never reuses the complete S1 review as complete S2 review.
- Update `team/review` in the catalog; an admitted parent Plan retains its pinned child,
  while a newly planned Task records the deliberately updated dependency.
