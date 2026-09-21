# Task execution: the next product increment

**Status:** design proposal. The product model is accepted in
[ADR-0046](../adr/0046-add-versioned-task-contracts-with-exact-plan-approval.md); the contracts,
syntax, defaults, and implementation sequence below are the proposal as written, not shipped
functionality. The kernel's ADRs from 0046 onward and [`../task-execution.md`](../task-execution.md)
record what was implemented and where it diverged.
**Baseline:** workspace version 0.8.0.
**Examples:** [proposed TOML](task-execution-examples.md).

## 1. Product promise and increment boundary

Afactory executes a Task through a reusable, testable way of working. A developer describes
the outcome; the team supplies Pipelines, Workers, and verification policy; Afactory selects
a compatible Pipeline or creates one, binds it to the developer's local capabilities, and
returns the result with evidence, cost, and history.

The product's reusable asset is a team's tested development process. One developer improves
a Worker or Pipeline, commits the definition and its tests, and colleagues can use it with
their own Providers. Teams can maintain cheap, fast strategies for bounded work and heavier
strategies for work requiring decomposition or additional evidence.

The first commercial use case is **a well-specified issue to a verified local change**:
import a Jira issue, execute a suitable strategy, inspect the resulting Snapshot and evidence,
and explicitly deliver it to a new local worktree. Jira is one input adapter, not the Task
model. A documentation Task is the second proving case: the model must represent non-code
outcomes without creating another scheduler.

This increment includes the common Task runtime, reusable Pipeline contracts, Worker binding,
automatic selection, bounded generated Pipelines, Git sharing, starter packs, and conformance
tests. It does not require a marketplace service, distributed scheduling, autonomous remote
publication, a visual editor, or an optimizer trained on production history. Existing local
delivery remains a separately authorized effect. Those limits keep the first product coherent.

Two decisions are explicit: **every newly generated Execution Plan requires developer
review and approval before execution**, even within existing project permissions; sharability,
batteries included, and pluggability supplement the unchanged engineering priority order,
led by token economy and minimum Worker context. Plan approval preserves existing authority,
budgets, and verification obligations.

## 2. Use the database analogy precisely

The useful comparison is the separation between intent, planning, binding, and execution.
PostgreSQL produces candidate paths before building the plan passed to its executor.
[PostgreSQL planner documentation](https://www.postgresql.org/docs/18/planner-optimizer.html)
describes that boundary. Calcite permits directly constructed operator trees and extensible
planning rules and metadata; SQL text is not necessary to use that model.
[Calcite algebra documentation](https://calcite.apache.org/docs/algebra.html)

| Database concept | Proposed Afactory counterpart |
|---|---|
| Submitted query | Task contract: required outcome, inputs, acceptance, constraints |
| Catalog and statistics | Locked Pipeline/Worker/Tool catalog, capability facts, measured execution history |
| Reusable parameterized expression | Git-versioned Pipeline definition with named inputs and applicability |
| Physical execution plan | Immutable Execution Plan: one Task revision plus exact bindings and policy |
| Executor | Shared Task runtime and typed operator scheduler |
| EXPLAIN | Why a Pipeline was selected; rejected alternatives, bindings, estimates, obligations |
| EXPLAIN ANALYZE | The actual recorded execution: attempts, timings, spend, evidence, outcome |

This is an architectural analogy, not a claim that model work has relational equivalence.
Two model strategies can produce different valid results. Afactory can prove structural
properties and check acceptance; it cannot generally prove that two prompts are semantically
equivalent or that the cheaper plan will succeed. First-increment optimization is therefore
deterministic filtering and policy ranking. Estimates show provenance and uncertainty.

## 3. One Task model, several Task kinds

**Task is the durable unit of intent, authority, budget, history, and outcome.** Every business
workflow enters through it. `review`, `implement`, and `document` are Task kinds with distinct
input/output and acceptance contracts, not separate execution engines. Difficulty, provider,
cost preference, and Pipeline name are not Task kinds.

A Task contract contains:

- an immutable revision ID, goal, kind, and provenance;
- exact typed inputs, including a Subject or source Snapshot where relevant;
- required outputs and acceptance obligations, identified individually;
- trusted authority, allowed effects, scope, and data destinations;
- token/cost limits where enforceable, wall deadline, and reserved verification capacity;
- strategy preference, an optional explicit Pipeline, its `fallback = refuse | select | generate`
  setting (default `refuse`), and declared task facts.

Free text, stdin, TOML, an issue connector, or an API may create the same contract. Missing
acceptance information is an explicit unresolved obligation. A classifier may propose facts;
its claims cannot authorize tools, weaken verification, or establish an unverified fact as
true. Unknown complexity or scope chooses a compatible conservative strategy or asks for
the missing information.

A Task kind package declares its input/output schemas, root-adapter bindings, minimum verification
requirements, and display semantics. Before selection, its locked adapter resolves artifact IDs,
validates their recorded types, and materializes permitted defaults into a captured input manifest.
An artifact name is not its type: `input:imported-issue` is an ID, never an implicit subtype of
`task-requirements`. Issue import must produce a normalized, versioned requirements artifact with
provenance; any conversion is a declared catalog operation, recorded before matching.

| Implementation root port | Permitted binding |
|---|---|
| `source: snapshot` | Required exact Task-declared Snapshot artifact |
| `requirements: task-requirements` | Required Task-declared normalized requirements artifact |
| `review_history: review-history` | Exact Task review lineage, or a typed empty artifact only when the port explicitly declares `root_default = "empty-review-history"` and the lineage has no history |

Root defaults are a closed list of deterministic constructors admitted by the Task kind and
trusted policy. A generated Pipeline cannot invent them or synthesize a missing source or goal.
Selection checks this captured binding manifest against each interface; a required unbound port
rejects that candidate. Embedded calls bind every required port explicitly and do not use root
defaults. Resuming an existing lineage cannot replace its history with an empty artifact.

The first implementation supports manifest-only kinds over existing
operators; arbitrary new executable operators are an explicitly installed extension.

`af review ...` remains a convenience surface over `kind = "review"`. Review's Campaign,
Round, Subject, Finding, Demand, and convergence retain their kernel meanings. An implementation
Pipeline can call a reusable review subpipeline without inventing a second top-level Task.
A child Task is warranted only for independently owned acceptance/budget/lifecycle; this
increment does not require automatic child-Task decomposition.

Control-plane commands such as help, install, configuration inspection, and reading a Task
do not create business Tasks. Pipeline steps become nodes and Attempts, not thousands of
unnecessary Task records.

### Separate completion from the domain conclusion

The common result records three axes:

| Axis | Examples |
|---|---|
| Execution | completed, incomplete, blocked, exhausted, cancelled |
| Acceptance | satisfied, unsatisfied, inconclusive |
| Typed result | verified Snapshot; complete review with changes requested; checked document |

A completed review that found defects may satisfy a Task whose goal is to produce a complete
review. It does not approve the change. If the Task instead requires a converged review,
those defects leave acceptance unsatisfied. Always render the domain conclusion prominently:
`Review complete; changes requested`. The legacy `af review` exit status and Finding
obligations remain intact. A generic Task success must never masquerade as review approval.

## 4. Pipeline definition versus Execution Plan

**Pipeline** means the reusable file: a versioned, parameterized graph of typed operators,
named ports, bounded control flow, Worker slots, outputs, applicability, and verification
coverage. Humans and a Planner produce the same language. A generated Pipeline is inspectable
TOML, not an agent's hidden to-do list.

**Every Pipeline has a versioned public input/output contract.** This applies equally to
implementation, review, documentation, shared definitions, generated definitions, and every
embedded Pipeline. Review uses the common Pipeline abstraction; it has no special composition
interface. Each contract declares named ports, schema versions, required/optional cardinality,
Snapshot/Subject affinity where relevant, and the domain meaning of its outputs. A complete
definition binds each public output to a compatible internal producer. A missing or incompatible
contract is a compilation error, including for a newly generated Pipeline.

Evidence-bearing public outputs also declare the acceptance obligations they `cover`, keyed to
trusted verifier/check identities and their required Subject affinity. The compiler proves this
claim from internal producers when it validates the child, then checks the parent against the
child's public coverage. A broad `check-result-set` type alone proves no named obligation.
Removing a covered obligation is a public-contract compatibility change and invalidates a caller
that requires it. Runtime still requires complete, successful receipts for each named obligation;
a declared coverage path is not a passing result. Public verification results retain references
to the exact check/evaluator evidence they summarize.

Root Task adapters and parent Pipeline calls bind the same public input ports. Internal nodes
read those ports rather than implicitly reading the enclosing Task's business inputs. Trusted
execution context (Task identity, authority, budget, cancellation, and event lineage) is inherited
separately and cannot be enlarged by a data input. Task-kind applicability selects root
Pipelines; it is separate from the interface used for composition.

The runtime returns a common execution/acceptance envelope alongside the declared domain
outputs. Required outputs must exist on completed paths, including a completed negative result
such as `changes_requested`. Incomplete, blocked, exhausted, or cancelled paths explicitly
identify unavailable outputs; dependent nodes cannot consume them as empty or successful values.
The compiler checks exhaustive terminal handling and runtime validation checks actual artifacts
against the contract. Schema compatibility permits a call; evidence determines acceptance.

```text
Task adapter OR parent call
           |
           v
  +-------- Every Pipeline --------+
  | public typed inputs           |
  |             |                 |
  | operators / Workers / calls   |
  |             |                 |
  | public typed outputs          |
  +-------------------------------+
           |
           v
caller receives outputs + execution/acceptance envelope
```

**Execution Plan** is the recorded compilation of that Pipeline for one Task revision. It
pins the complete dependency closure, Worker packages and effective settings, Provider
capabilities, environment requirements, input identities, budgets, and acceptance mapping.
It is a derived artifact, not a seventh category of human-authored configuration.

```text
Task definition
      |
      v
Resolve contract + trusted policy + exact inputs
      |
      v
Find compatible Pipelines in locked catalog
      |
      +-- compatible --> rank --> select shared Pipeline --+
      |                                                   |
      +-- none --------> bounded Planner Worker             |
                            |                              |
                            v                              |
                      generated Pipeline TOML -------------+
                                                           |
                                                           v
                                              compile + bind + validate
                                                           |
                                                           v
                                             persist immutable Execution Plan
                                                           |
                                                           v
                                               generated definition included?
                                                 | yes             | no
                                                 v                 |
                                           developer review        |
                                                 | approved        |
                                                 +-----------------+
                                                           |
                                                           v
                                               admit + common Task executor
                                                           |
                                                           v
                                               result + verification + usage
```

The compiler checks:

1. Port type, cardinality, Snapshot affinity/lineage, required inputs, and complete outputs.
2. Every Task acceptance obligation has a verification path over the final output.
3. Graphs are acyclic after subpipeline expansion; retries and Scatter are explicit bounded
   operators. No arbitrary backward edges or unbounded expansion.
4. Worker bindings satisfy role, contract, capability, independence, and environment requirements.
5. Policy permits every effect and data destination; exact configuration cannot come from
   untrusted candidate content.
6. Resource envelopes include planning, workers, retries, tools, and verification. Impossible
   budgets are refused before dispatch; deadlines also apply to planning and waiting.

A fixed compiled graph may contain bounded branches, retries, and Scatter. A new topology
requires a new immutable plan revision at a recorded barrier. Persisted plans are never edited
in place. The declared control structure replaces the need for arbitrary user-written
state-machine code while preserving the existing bounded dynamic Scatter mechanism.

For the first increment, semantic repair is a **statically expanded bounded composition**.
A definition may request one repair of an implement/seal/check/evaluate subpipeline; compilation
creates namespaced first-pass and repair nodes plus typed conditional edges. A failure receipt
activates the repair path, which consumes the previous derived Snapshot and exact feedback.
A deterministic select operator returns the accepted Snapshot, or the final unsatisfied result
after the bound. Conditional ports have declared zero-or-one cardinality and exhaustive
terminal handling. All possible nodes and their worst-case budgets are visible in EXPLAIN.
Transport retry of one Worker and semantic repair of a failed result remain different actions.
This supplies the worked repair example without changing a running graph or adding a coordinator.
Review-driven repair additionally expands `review-bind`, `attest-fixes`, and `fix-verify` as
specified below; generic reevaluation alone cannot discharge a Finding.

### Composition example: an embeddable Review Pipeline

**Clarification:** a Review Pipeline must be usable both as the top-level
Pipeline of a review Task and as an embedded Pipeline inside implementation. Every Pipeline
has the same input/output contract requirement; Review is the worked example. Embedding is a
required feature of the increment, not an optional heavy-only feature.

The same locked `team/review` definition is used in both contexts:

```text
Task(kind=review) ------------------------------> team/review

Task(kind=implement) --> team/implement-reviewed
                              |
                         implement --> seal --> call team/review --> accept
```

A Pipeline call is a typed reusable graph boundary. The compiler expands the child into
namespaced nodes, while inspection retains the hierarchy. The parent does not shell out to
`af review run`, create another Task, start an independent budget, or recapture live HEAD.
Root Task-kind applicability controls top-level selection; an embedded call is validated
against the child's public port contract. A review-only root selector must not prevent an
implementation Task from calling its review graph.

| Review input | Binding in an implementation Task |
|---|---|
| `subject` | Diff Subject derived from immutable source S0 and sealed implementation S1 |
| `requirements` | Exact Task requirement artifacts declared relevant to review |
| `prior_findings`, `prior_demands` | Exact Subject-bound Sets from the review lineage, including typed empty Sets initially |
| Authority, mode, and Worker slots | Trusted pinned child policy, intersected with parent authority and mapped slot bindings |

| Review output | What the parent may conclude |
|---|---|
| `checks` | These named checks ran on this exact candidate under this authority |
| `findings`, `demands` | Durable claims and obligations to retain and address |
| `assessment` | Completeness plus a domain conclusion, exact Subject, scope, and evidence |

The parent consumes a typed assessment, not the fact that the child process returned zero.
Acceptance requires the requested review scope to be complete and blocking obligations to
be discharged under trusted policy. `changes_requested`, incomplete review, and a clean
review are distinct. A review Task can complete with findings, while an implementation Task
consuming those findings remains unverified.

The example reviewed implementation Pipeline lets the child own the required unit/API Gates,
then passes its check receipts to final acceptance. This avoids executing the same Gates
twice. An alternative can pass existing receipts only through an explicit child input contract
that validates check authority, candidate identity, environment, and freshness.

On findings, a declared repair creates S2 and preserves the original claims. A light review
remains one closed Round. Its repair path explicitly rebinds the review lineage to S0-to-S2,
runs current-S2 Gates, attests changed regions, and invokes the `fix-verify` operator before
final acceptance; it does not start a replacement light Campaign.
A configured heavy strategy may rerun the same Review Pipeline on S0-to-S2 under the same
review lineage and captured round bound. Changing a candidate never makes S1 receipts valid
for S2, and absence of a repeated finding is never enough to mark it fixed.

| Repair operator | Required inputs | Outputs and authority |
|---|---|---|
| `review-bind` | S0, S2, exact prior Finding/Demand views and lineage | Current Subject S0-to-S2 and rebound views retaining original claim identity, origin Subject and scope |
| `attest-fixes` | Current Subject/views, sealed S1-to-S2 Change Set, proposed per-Finding changed regions | Change Attestations keyed to the current views and exact S2; a claimed change is not a verified fix |
| `fix-verify` | Current Subject/views, Change Attestations, current Gates | Per-Finding positive/negative/inconclusive Fix Verification receipts, updated views, and a typed repair assessment; an independent verifier Worker supplies the evidence |

The kernel validates every Finding/view, Attestation, Subject, policy and evidence reference
before applying a positive resolution. Missing or stale verification leaves that obligation
open. `review-bind` records a verification continuation with an exact Subject transition;
it does not rewrite the closed S1 Round or consume a new discovery Round. This is a required
new Task-runtime contract in T0/T1, not a claim that the current `af review` CLI can advance
its Subject through `attest-change` alone. Historical review records stay immutable.

A repair assessment means the named fixes were verified; it does not claim a fresh complete
review of S2. Final acceptance may use it only when trusted policy admits that targeted scope
for these repair changes, alongside current Gates and independent final evaluation. If the
Task requires a complete new review of S2, or repair scope exceeds that policy, this light
strategy leaves acceptance unsatisfied/inconclusive and reports the missing review obligation.
Only an explicitly admitted strategy with another discovery Round can satisfy that obligation.

All child Attempts, planner work, repair, and verification debit the parent Task and any
tighter child/node limits. Child slot names are qualified (for example,
`review.correctness`); parent call bindings map them to reusable Worker packages. Independent
review/evaluation rules are checked against the parent implementer as well as siblings inside
the child. Child failure and cancellation propagate as typed outcomes without losing receipts.

Composition must support the full sharing promise: upgrading the Review Pipeline changes a
locked dependency deliberately, and the next admitted parent Plan records the new digest.
Already admitted parent Plans retain the original child bytes. Both shared and generated
implementation Pipelines can call the same Review Pipeline.

## 5. Select before generating

Applicability is an explicit contract: supported Task kinds, input types, task facts, output
types, scope bounds, required toolchain/capabilities, and acceptance coverage. A label such as
`cheap` is a preference, not evidence that a Pipeline fits.

Selection is deterministic over pinned inputs and the catalog snapshot:

1. If the user selects a Pipeline, validate it and use it when compatible and feasible.
   Otherwise return exact reasons. Its Task-level `fallback = "refuse"` stops here;
   `select` or `generate` enters the ordinary catalog selection steps below. Neither bypasses
   an existing compatible definition, and neither relaxes facts, authority, or budgets.
2. Otherwise filter catalog entries by the complete applicability contract. Unknown required
   facts do not count as matches.
3. Resolve permitted local bindings and filter by declared capabilities and budget feasibility,
   including the protected verification allocation. Retain a rejection reason for every
   discarded candidate. If the preferred candidate cannot fit but another can, keep the latter.
4. Apply project routing and preference order among feasible candidates. Distinct ties that
   policy has not ordered produce an ambiguity result, not a model's hidden choice.
5. Generate only when no applicability-compatible definition exists and trusted policy permits
   it. With an explicit Pipeline, `fallback = "select"` stops with no-match and `generate`
   requests this last step. Without an explicit Pipeline, project `no_match` chooses refuse
   or generate. Both paths require the same trusted planning authority and limits.
   Compatible definitions that are all over budget return `infeasible_budget`; missing
   installed capabilities or Provider setup return `unavailable`. Neither is disguised as
   semantic no-fit. A future policy may permit generation for cost reduction, but it is not
   an automatic first-release fallback.

EXPLAIN uses declared capabilities and recorded observations and labels authentication that
has not been probed. Actual Provider admission occurs under the selected Plan. An admission
failure returns `blocked`; selecting an explicitly allowed alternate binding produces a new
recorded Plan before dispatch. It never silently changes the already admitted binding.

For the first release, declared cost class plus trusted preference order determines ranking;
measured token and latency ranges inform EXPLAIN without pretending to predict success exactly.
Later cost ranking must consider total cost per independently verified result, not only the
price of the first model call. Historical quality samples are keyed by Task class, Pipeline,
Worker, evaluator, provider/model, and verification suite digests; unrelated samples are not
pooled as proof of equivalence. Missing statistics are shown as unknown.

A reused Pipeline avoids planning model calls. It does not imply reusing previous model
answers. Cached deterministic artifacts require exact inputs, authority, operator version,
environment identity, and freshness policy; verification against a changed Snapshot reruns.

## 6. Dynamic generation and its lifecycle

The Planner is an ordinary reusable Worker with a typed `PipelineProposal` output. Its
bootstrap is a fixed kernel planning sequence using the same Attempt accounting and execution
boundary as other Workers. It does not recursively ask an unplanned Task to plan itself.

Before the first Planner Attempt, persist a small **bootstrap Execution Plan** and its
PlanAdmitted receipt, pinning the Planner binding, catalog, exact inputs, authority, repair
bound, and Task budget reservation. It uses the fixed planning operator sequence rather
than invoking selection on itself. The generated execution Plan names that bootstrap Plan
as its cause. A crash between planning and final-plan admission resumes the captured proposal
and receipts, not a fresh paid Planner call.
This bootstrap uses an existing trusted planning definition. The mandatory developer approval
gate applies to its generated execution proposal, so planning can produce a reviewable result
without recursively requiring approval of a not-yet-generated plan.

Its exact input is the Task contract, unresolved requirements, bounded catalog descriptions
and signatures, trusted effects policy, and budget. It may retrieve specific package details
through journaled bounded tools. It does not receive other workers' transcripts or the entire
repository by default.

The Planner may compose installed operators, Workers, and subpipelines, bind parameters,
and choose an allowed strategy. It cannot create executable plugins, download dependencies,
install commands, invent acceptance criteria as already approved, change trusted checks,
or grant itself additional capabilities. New capabilities return `blocked` with the missing
dependency. The model proposes structure; the compiler and policy decide admissibility.

Proposed default bounds: two Planner Attempts total, one repair after structured compiler
feedback, 64 expanded static nodes, subpipeline depth four, no more than 16 concurrent shard
slots, and a Task-level planning token cap. Project policy may tighten these. Raising them
requires authority that the Planner cannot supply. Repair and failed generation consume the
same Task budget; generation stops with a typed explanation when exhausted.

```text
proposed TOML --> compile/bind/validate --> persist exact plan
      |                  |
      |                  +-- errors --> one bounded repair --> stop if still invalid
      |
      +--> stored exact definition and provenance
                       |
              explicit export as reusable files
                       |
             .af/pipelines/<name>.toml + contract fixtures
                       |
                  review + Git commit
                       |
              reusable by the whole team

persisted generated plan --> wait for developer review
                                        |
                            +-----------+-----------+
                            |                       |
                         approved                rejected
                            |                       |
                     admit --> execute        no execution
```

Every generated Pipeline is durably stored as exact TOML before execution and exposed through
Task inspection together with its compiled Execution Plan. **Developer review and approval are
mandatory for every generated plan.** This is a human decision by an authorized project
developer; model review can provide advice but cannot approve its own proposal. The Task waits
with `reason = "needs-plan-review"`; no work from the generated graph is dispatched while the
approval is absent. Shared children inside that graph wait with the parent.

The review shows the goal, exact inputs, generated TOML, expanded graph including embedded
Pipelines, Worker/provider bindings, effects, verification coverage, budgets, and estimates.
Approval records the developer identity, exact Task revision and Execution Plan digest, and
policy revision in the Store. That digest covers the input identities, full dependency closure,
effective bindings, authority, budgets, and required verification. Changing any covered input
or field creates a new plan and requires another review. Rejection is durable and prevents
execution; a revised proposal requires its own approval. A valid approval survives a crash
only for that same immutable plan, so resuming it need not ask again.
Producing intermediate artifacts or expanding an already-declared bounded Scatter executes the
approved plan; it does not itself change that plan. Replacing its graph or external input bindings does.

Admission rechecks approval, current permitted authority, remaining budget, and deadline before
dispatch. Approval never grants extra permissions, drops a Gate, replenishes spend, or extends
the deadline. Expired or revoked authority blocks execution. Silence or elapsed time is not
approval; the Task's hard deadline continues to include waiting and may expire before dispatch.
No project/local/generated setting may enable automatic execution of a generated proposal.

The gate applies when the final plan contains newly generated definitions at any nesting depth,
including a generated child of a shared Pipeline. A Pipeline deliberately reviewed, tested,
and promoted into the trusted Git catalog follows normal shared-Pipeline policy on later Tasks;
compiling that existing definition does not itself create a new generated proposal. Reusing a
previous Task's approval never authorizes a newly generated plan for another Task revision.
Export alone never clears the originating Task's pending plan approval.

Runtime creation does not silently modify a team's active Git catalog. `af pipeline export`
writes a portable definition at an explicit absent destination. It generalizes Task-specific
values into typed parameters and validates the result; it excludes ticket contents, secrets,
machine paths, account names, and ephemeral IDs. User review and normal Git publication make
it team policy. Definitions are shared; executions, logs, and Task state remain in the Store.

After dispatch, replanning is only allowed at a barrier where prior Attempts have settled
or been fenced. The new plan names its predecessor, cause, inherited budget spend, and exact
reusable outputs. It must preserve acceptance or create an explicitly authorized Task revision.
Every newly generated replacement plan requires fresh developer review before its nodes dispatch.
Initial delivery needs pre-execution generation; automatic mid-execution replanning is deferred
until these invariants have fixtures.

## 7. Workers are portable packages; bindings are local

A Worker definition bundles role, versioned input/output contract, instructions, allowed
tool requirements, environment minimum, configurable settings, defaults, and contract tests.
It is reusable across Pipelines and repositories. An Attempt is an execution of that definition;
the package carries no mutable conversation state.

Pipelines bind **slots** such as `implementer`, `correctness`, or `evaluator`, with required
contracts and a default Worker reference. A developer may supply a different Worker package or
Provider/model/effort combination for a permitted slot. Multiple Pipelines can refer to the same
Worker; upgrading the catalog pin changes it deliberately for all those consumers.

```text
Shared Pipeline             Shared Worker package           Developer machine
---------------             ---------------------           -----------------
slot: implementer --------> role + typed contracts <--------- local Worker replacement
slot: evaluator   --------> instructions + tests   <--------- provider/model/effort
                            tool/env requirements <--------- available local adapters
                                      |
                                      v
                            validate effective binding
                                      |
                                      v
                       exact Worker + settings in Execution Plan
```

Ordinary config layering resolves preferences, but authority is an intersection of mandatory
constraints. A later config layer cannot remove a required Gate, enlarge data destinations,
lower isolation, exceed a cap, or make an implementer its own evaluator. After role/type checks,
`independent_from` requires distinct effective Worker package digests, fresh isolated Attempts
and sessions, and role-specific contexts without the implementer's private conversation state.
By default it also requires distinct authenticated Provider principals, matching the previous
independent-evaluator intent. Compare canonical principal identities, not registry aliases;
unknown identity cannot establish that stronger requirement. Trusted project policy may explicitly
allow the same principal while retaining package/session/context separation, or require stronger
provider/model diversity. Local bindings and generated definitions cannot relax that policy.
Two contract-compatible slots bound to the same multi-role package therefore fail independence;
two different packages bound through aliases of the same principal fail the default principal rule.
These are enforceable structural boundaries, not proof of statistically independent judgments.

Local instruction experiments are valid Worker replacements whose full effective package is
captured and tested. They do not silently redefine a shared package digest. `af worker export`
lets a developer publish the portable replacement after removing local bindings. A shared
`ci` profile fixes approved bindings; local optimized bindings remain identifiable in results.
Sharing a Pipeline does not promise identical model output across developers.

## 8. Git catalog, batteries, and plugins

Shared files are human-readable TOML plus Markdown instructions and fixtures:

```text
project/.af/
  af.toml                     Task policy, catalog imports, strategy preferences
  af.lock                     engine + transitive package digests and Git commit pins
  pipelines/                  reusable Pipeline definitions
  workers/                    reusable Worker packages
  kinds/                      optional Task kind contracts
  profiles/                   team strategy/binding presets
  tests/                      Pipeline and Worker conformance fixtures

machine configuration         Provider auth contexts and private binding preferences
Store                         Tasks, plans, Attempts, events, outputs, generated definitions
```

Catalog references have explicit namespaces, such as `builtin/implement-small`,
`team/implement-heavy`, and `project/docs`. Import a Git repository at an exact commit and
path; record the transitive dependency closure and byte digests in the lock. There is no
implicit remote discovery or floating branch resolution during execution. Duplicate names,
cycles, unresolved digests, and unsupported schema versions fail with actionable diagnostics.
`af catalog sync` fetches the declared closure; cached locked definitions support offline
planning. Upgrade is a reviewed lock diff. Copying files remains a valid simplest sharing path.

The first starter pack supplies `review-light`, `review-heavy`, `implement-small`,
`implement-heavy`, and `document`, plus Planner, Implementer, Correctness Reviewer, and
Evaluator Worker definitions, command-based fixtures, and language/toolchain Gate recipes.
Heavy presets declare their extra scope and reserve; a cheap preset never deletes the Task's
required acceptance. If the cheap strategy cannot fit, Afactory selects another allowed
strategy or explains the budget/scope mismatch.

Onboarding installs or references this pack, discovers usable toolchain checks, explains
missing Provider setup, and previews the policy it will create. No model work or paid account
is required to run the command-based tutorial and contract fixtures. A real model Task still
requires an admitted Provider; do not promise anonymous hosted inference.

Pluggability has three practical levels:

| Extension | What is shared | Execution boundary |
|---|---|---|
| Pipeline, Worker, Task kind, profile | Versioned files and fixtures | Same parser/compiler/contracts |
| Source, Provider, Tool, environment adapter | Explicitly installed executable + manifest | Versioned JSON protocol, typed capabilities and effects |
| New operator/reducer | Explicitly installed trusted plugin + schema + conformance tests | Same admission, receipts, replay, and budgets |

The first increment must prove a replacement Provider and a replacement source adapter without
changing the scheduler. It need not ship a universal plugin host. No arbitrary plugin code
runs inside the coordinator process. Project imports cannot implicitly install executables.
External operations carry versioned request IDs, cancellation/deadline support, bounded I/O,
normalized usage/outcomes, and durable receipts. Worker files and live provider sessions remain
different concepts.

## 9. Runtime, data flow, and recovery

Reuse the existing typed DAG scheduler. Extract shared Attempt execution and node handlers;
port implementation/seal/check/evaluate into it. The Task coordinator owns intent and lifecycle,
the planner compiles one definition, and the scheduler decides what is ready. Do not add a third
orchestrator over the two current command drivers.

Operators consume explicit artifact references through ports. In particular, an implementer
produces a **new** Snapshot; acceptance and evaluation nodes bind to that output, not to a
single global source Snapshot. Review operators retain Subject affinity, per-claim scope,
Finding/Demand completeness, and whole-Subject closure after Scatter. This per-node input
binding is essential to unification.

The initial operator families are capture/bind (including `review-bind`), Worker invocation,
seal, check, review reduce, `attest-fixes`, `fix-verify`, verify/evaluate, bounded Scatter/gather,
subpipeline call, and recorded outcome. Reuse existing
typed review artifacts rather than widening them into untyped JSON blobs. Subpipeline calls
have versioned public ports, compile into namespaced nodes, and share the Task budget.

The Task lifecycle is:

```text
submitted --> resolving --> planning --> ready --> running --> verifying --> completed
                  |            |                    |             |
                  +------------+--------------------+-------------+
                               |
                    waiting(reason=needs-input | needs-human | needs-plan-review)
                               |
                        resume pinned phase

Any active phase may stop as: incomplete / blocked / exhausted / cancelled
```

Completion includes the separate acceptance and domain-result axes from section 3. An
unsatisfied terminal result is recorded; an authorized strategy may propose a new plan
revision with remaining budget. Resume reconstructs the exact plan and receipts, never
re-runs the Planner just because the process restarted.

`waiting` is resumable, not a terminal result. `needs-human` is its reason when an external
effect cannot be reconciled automatically; `needs-input` covers missing Task information;
`needs-plan-review` holds a persisted generated plan until developer approval.
Neither authorizes a side effect or restarts a completed review Round. If a CLI invocation
ends while waiting, it reports the pause and preserves the resumable Task.

The review adapter derives its legacy exit code from the recorded review verdict, never from
generic Task acceptance alone. The mapping is exhaustive by precedence:

| Review invocation outcome | Task execution / acceptance | `af review` exit |
|---|---|---|
| Usage error before admission | No Task result | 2 |
| Operational error before a review verdict can be recorded | No completed review result | 1 |
| Required review node missing, including timeout/cancellation or a paused/blocked invocation | Incomplete/blocked/cancelled/waiting; acceptance cannot be satisfied | 4 (`Incomplete`) |
| Complete review with findings, or captured convergence Round limit reached | Completed, or exhausted by convergence policy; acceptance depends on the Task goal | 3 (`Fail`) |
| Complete review satisfies the captured light/heavy review policy | Completed; review acceptance satisfied | 0 (`Pass`) |

Resource exhaustion that leaves a required node missing follows `Incomplete` (4); a complete
finding-bearing light Round follows `Fail` (3), even if producing that review satisfies the
generic Task goal. Invalid combinations such as incomplete execution with satisfied acceptance
are refused. A verifier timeout is never rewritten as a completed inconclusive review. Kernel
migration fixtures must retain the existing Pass/Fail/Incomplete precedence and error behavior.

Persist definition/plan artifacts and a PlanAdmitted event before dispatch. Reserve before
each Attempt, journal admission before execution, fence abandoned epochs, and retain late
receipts without letting them satisfy the Task. All operator output and final-verification
receipts refer to the exact input and Snapshot identities.

Verification capacity is a protected allocation, not a reporting field. Before planning,
hold the Task kind/policy's minimum verifier allocation; planning, implementation, and repair
cannot draw from it. Final-plan admission proves that the selected verifiers' minimum
reservations fit this allocation and increases it before admitting other execution work when
required. An infeasible allocation refuses the Plan before implementation. Replanning carries
forward all charges and commitments; it cannot reset this protection or the Task's total.
After failure, no unverified worker may spend the protected allocation merely to try again.

The protection covers Attempt counts and required deadline capacity as well as tokens/money.
Pipeline `max_attempts` counts every model Attempt, including failed transport retries and
child/verifier calls. Admission reserves the minimum Attempt counts of all still-required
verifiers, and checks declared per-slot maxima against the total. An unprotected retry is
admitted only if its additional count and other resources leave that reservation intact.
For example, a total of three with implementer maximum two and evaluator minimum/maximum one
allows one implementation retry and still one evaluator call. With a total of two, that retry
is refused; the runtime cannot promise a final result when no implementation Snapshot exists.
Conditional repair paths reserve their required verification capacity before repair dispatch.

Reservations bound admission. If a Provider reports usage above its reservation, record and
charge the whole observed spend, emit `budget_breached`, and stop new dispatch under that
budget, including verification that no longer fits. A reservation is not a guarantee against
unbounded external billing; a strict physical cap requires an adapter with an enforceable
upper bound. Reports distinguish the admission cap, reserved capacity, and actual spend.
Budget exhaustion may leave a useful artifact unverified; it must never produce a false pass.

One local SQLite-backed Store owns the new Task lifecycle and an artifact CAS. Existing review
and implementation stores are read through versioned adapters during migration. One Task view
may link an existing Campaign; it does not duplicate or rewrite the Campaign log. A single
writer lease per Task and transactional event batches protect admission and settlement.
Crash recovery must not assume external effects are exactly-once: repeat only idempotent
operations, reconcile using receipts, or return `needs-human` where the effect is uncertain.

## 10. Verification is part of every strategy

Each acceptance obligation maps to a named verifier, expected evidence type, and exact final
output. Structural admission proves this mapping exists; runtime evidence determines whether
it holds. Passing a schema is not proof that code is correct or prose is true.

Implementation strategies use trusted deterministic Gates and an independent evaluator.
For non-code Tasks, substitute appropriate checks: document schema, cited-source coverage,
link validity, required facts, and policy-defined evaluation. If acceptance is subjective
or evidence is unavailable, report that limit rather than an unqualified verified result.

Task-proposed tests may be additional evidence; they cannot replace or edit mandatory trusted
checks. A Planner may add obligations but cannot erase the original ones. Generated Pipelines
undergo the same static contract checks as shared definitions. Exported reusable Pipelines
also need their own positive, rejection, and failure-path fixtures before team admission.

Developer optimization is measured against the same task cohort and verifier versions.
Record total paid tokens including planning, retries, and failures; available money with
reported/estimated provenance; time to verified result; verification failures; and human
interventions. The ranking objective is constrained cost/time with acceptance preserved.
No quality score may be derived solely from a Worker's self-assessment.

## 11. Proposed user experience

All commands here are design sketches. Existing 0.8.0 commands keep their current semantics
until the compatibility slices land.

```sh
af task create --from jira:ENG-142 --strategy fast
af task explain TASK_ID                    # deterministic selection/diagnostics; no model calls
af task plan TASK_ID                       # bounded generation only if needed and authorized
af task explain TASK_ID --plan PLAN_ID     # inspect the exact persisted proposal
af task approve TASK_ID --plan PLAN_ID     # developer approves a generated plan after review
af task run TASK_ID
af task show TASK_ID
af task explain TASK_ID --actual           # recorded timings, spend, failures, verification
af task deliver TASK_ID --branch af/ENG-142 --worktree ../ENG-142 --confirm TASK_ID

af pipeline export TASK_ID --name implement-api --directory .af/pipelines
af pipeline test .af/pipelines/implement-api.toml
af worker test .af/workers/my-implementer
af catalog sync
```

`af task start` is create + plan + run. When it generates a plan, it pauses at
`waiting(needs-plan-review)` and names the exact plan to inspect and approve. An unattended
invocation returns that pending state; it cannot silently approve or execute the proposal.
`explain` reports matching candidates, rejected requirements, local overrides, required
verification, and known/unknown estimates. It does not silently pay for generation.
`plan` may dispatch the bounded Planner and records that spend before execution begins.
`approve` records authorization but does not itself dispatch work; `run` resumes only the
approved immutable plan after admission checks.

Jira import snapshots the exact issue fields into Task input, records provenance and field
revision, and maps them into a proposed contract. Issue text is data, never execution
authority. Importing a changed issue creates a new Task revision; it cannot mutate a running
plan. The first connector is read-only. A local TOML/JSON import provides the same contract
offline and supports the full tutorial without a Jira account.

## 12. Worked product flow and failure branches

For `ENG-142: add a bounded pagination limit`, suppose trusted policy requires unit tests,
API compatibility checks, and independent acceptance against the final Snapshot.

1. Import records the goal and acceptance fields. The repository supplies trusted scope and
   check bindings. The Task has a ten-minute target and a hard token-admission cap.
2. `team/implement-small` accepts the known scope and covers all three obligations.
   Selection reuses it with zero Planner calls.
3. Alice binds its implementer slot to her optimized Worker; Bob uses another admitted local
   binding. Both Plans retain the shared Pipeline digest and distinct effective Worker digests.
4. The implementer changes a sandbox. Seal produces Snapshot S1. Unit/API checks and the
   evaluator inspect S1. All obligations satisfied yields a verified result; explicit delivery
   materializes S1 in a new local worktree.
5. A task requiring a database migration does not fit the small Pipeline's applicability.
   If `team/implement-heavy` covers it, select that. Otherwise the Planner composes a
   migration-aware Pipeline from installed capabilities, including migration verification.
6. If no migration verifier exists, the Task is blocked with the missing requirement.
   The Planner cannot invent a passing result. If its proposal omits API checks, the compiler
   rejects it and supplies structured feedback for the one allowed repair.
   Once valid, the generated plan is persisted and waits for developer review. The developer
   inspects its expanded steps, bindings, permissions, verification and budget, then approves
   that exact plan before execution. A rejected or changed plan cannot use that approval.
7. If a runtime Gate fails, acceptance remains unsatisfied. A declared bounded repair may
   produce S2; verification runs against S2. Old evidence for S1 does not satisfy S2.
8. A process crash resumes the same admitted Plan. A user changing the local Worker config
   meanwhile affects future Plans, not the resumed one.
9. Export the successful generated definition with its applicability contract and fixtures.
   After a reviewed Git commit and catalog pin update, the next matching Task reuses it
   without invoking the Planner.

The second proving workflow, a release-note Task, consumes immutable change summaries and
produces a checked document through the same create/plan/run/show lifecycle. Its output is
not a fake code Snapshot and its acceptance is not a renamed code-review verdict.

## 13. Delivery sequence and release gates

| Slice | Concrete deliverable | Exit evidence |
|---|---|---|
| T0: contracts | Versioned Task revision, Pipeline, plan, developer approval, binding, operator and verification schemas; fixture corpus | Both positive and forbidden cases; old review/Task readers remain valid |
| T1: one runtime | Extract shared Attempt execution; compile review and implement into existing scheduler; per-node Snapshot ports | Same engine path, preserved review results, implementation checks final Snapshot, crash/fence/budget tests |
| T2: reusable catalog | Shared Pipeline/Worker files, typed slots, local bindings, lock closure, explain, starter pack | Same Review Pipeline runs standalone and embedded under one parent budget/history; two developers use different legal bindings; contract violations rejected |
| T3: selection | Applicability, coverage validation, deterministic ranking and explicit-choice diagnostics | Existing compatible definitions cause zero Planner calls; unknown facts, ties, missing auth, no-fit cases tested |
| T4: generation | Bounded Planner, same compiler, persisted generated TOML, mandatory developer approval, export and reuse | Generated Plan waits for review, dispatches only after exact-plan approval, and rejects stale/reused approval; second imported Task reuses exported definition |
| T5: first product | Read-only Jira source adapter, document Task, conformance commands and benchmark report | Issue-to-verified-worktree demo, non-code result, unchanged scheduler for second adapter, user-facing evidence and failure explanations |

T0-T2 are useful engineering checkpoints, but **T0-T5 together are the marketable increment**.
Do not call a renamed CLI or a third wrapper around the old drivers the completed product.
Do not require broad physical renaming of frozen `review-*` crates or persisted artifacts to
ship this change. Kernel contract decisions and migration details are recorded in the kernel's
ADRs as implementation proceeds; this document owns the product proposal.

Migration preserves existing Campaign IDs, Findings, dispositions, event sequence semantics,
artifact versions, CLI exit behavior, source/delivery boundaries, and pinned-release policy.
Reopened legacy executions retain their original authority and runner until equivalence is
proved. New executions move to the common runtime
behind a controlled compatibility cutover. No log rewriting or regeneration of old evidence.

## 14. Proving the product claim

“A Jira in ten minutes” is a target for a declared small-task class, not a promise for every
issue. Freeze at least 20 representative tasks before tuning: small code changes plus
documentation cases, with larger/no-fit and deliberately impossible cases measured separately.
Use independent acceptance fixtures and holdout cases that are not visible to the optimizer.

Compare existing Afactory 0.8.0, the new fast preset, the heavy preset, and at least two local
Worker bindings. Separate cold setup from warm execution. Report p50 and p90 time to verified
output, actual token/money usage and provenance, completion/acceptance rates, regressions,
planning-call rate, human interventions, and delivery time. Include developer-review waiting in
end-to-end latency and also report it separately from active execution. Keep failures and budget exhaustion
in the denominator; do not report latency only for selected successful tasks.

Proposed gate for the product claim: warm small-task median at most ten minutes; verified completion at least
as high as the frozen baseline; zero known regression escapes on the acceptance corpus; two
developers reproduce the workflow from shared files; and generated-to-shared reuse succeeds.
These are evaluation criteria, not measured results. Real provider work needs a separately
budgeted benchmark execution. “Verified” means the declared checks and evaluator accepted,
not that Afactory proves all possible behavior.

## 15. Decisions, assumptions, and unresolved work

Settled decisions: Task centrality; review as a kind of Task; configured or generated
Pipelines; shared Pipelines and Workers; local Worker tuning; cheap/fast and heavy strategies;
sharability, batteries included, and pluggability as product values supplementing the unchanged
engineering priority order; mandatory developer review and approval of every generated plan
before execution; public input/output contracts for every Pipeline and embeddable review.

Proposed here: bounded generation within trusted policy, explicit export into Git,
typed applicability and slots, a compiled Execution Plan, common operator execution,
conservative first-release ranking, the T0-T5 cut, and the product-claim gates.

Before T0 implementation, resolve the exact versioned syntax, supported type compatibility
rules, and operator protocol in kernel ADRs; use the examples as conformance cases. Verify
whether existing pipeline format versions can compile losslessly before allocating the new
format version. Select an initial public support matrix from actual provider/Gate probes.
These details do not block review of this product increment, and none is silently claimed
implemented by this document.
