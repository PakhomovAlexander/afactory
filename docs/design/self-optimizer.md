# Self-optimizer: project improvement through an AF Pipeline

Status: design, 2026-09-16, revised. It targets the common Task runtime at source
revision `166eca5`. Milestones M1–M3 of §10 ship: `af self optimize` captures
history, runs protected experiments and drives the light diagnose/propose/adopt
path with typed recipe, economics and adoption contracts. M4, the heavy
whole-Pipeline redesign, has not started, and live paid demonstrations remain
pending. The shipped behaviour is documented in
[Self-optimizer economics](../task-execution/self-optimizer.md); the binding record
is [`../adr/`](../adr/), and where this note and an ADR disagree, the ADR wins.

`af self optimize` should turn experience working in a project into a reviewable,
tested improvement to the project's Pipelines, Workers and harness. It is a normal
Task executed by a reusable Pipeline, with captured inputs, bounded Attempts,
independent acceptance and a durable result. It does not install a new AF binary.

The useful promise is: **show where work fails or wastes resources, propose a
change within the selected strategy, test it against the old configuration, and
retain the evidence for adopting or rejecting it.** A cheaper Worker call is not an improvement if more Tasks fail.

## 1. Product boundary and terminology

The additions below are proposed vocabulary; the existing terms retain the meanings
in [CONTEXT.md](../../CONTEXT.md).

| Term | Meaning |
|---|---|
| Observation | A normalized, source-addressed fact about an execution, intervention or outcome; not an instruction or verified causal explanation. |
| History Capture | An immutable, project-scoped collection of observations, source-prefix identities, exclusions and completeness metadata. |
| Optimization Policy | Project-owned limits on editable configuration, permitted experiments, required acceptance and objectives. Captured before diagnosis. |
| Optimization Proposal | A hypothesis and exact candidate configuration Snapshot, linked to supporting observations and a proposed validation method. |
| Experiment Specification | Exact cases, baseline and candidate closures, oracle, environment, repetitions, budget and approval requirements fixed before execution. |
| Optimization Result | A source-bound conclusion, proposal, comparison evidence and adoption instructions; it does not activate configuration. |

In v1, optimization means improving project-owned `.af/` definitions and explicitly
allowlisted harness files. Harness means the project's build/check scripts,
dependency preparation, test fixtures and execution wrappers. AF's process
supervisor, adapters, credential registry, global agent settings and installed
binaries are outside the writable target. Findings about them become upstream
recommendations with reproductions. Product feature code is not an optimization
target merely because a log contains a feature failure.

Changes to the optimizer's own Pipeline may be proposed for a subsequent run, but
the current run's optimizer closure, collection policy and acceptance authority
remain frozen. An optimizer cannot evaluate itself under its replacement rules.
Changes to the Optimization Policy or the independent acceptance harness are
separate proposals requiring owner review; they cannot qualify themselves.

## 2. User experience

The command resolves the current project and a configured `optimize` Task-kind
package. Its first invocation performs bounded, token-free capture and displays the
normal Task plan preview. This follows the preview implementation present at the
target revision. ADR-0104 still says Proposed: adoption of that decision is an
explicit prerequisite, not assumed accepted architecture. Task inspection,
confirmation, cancellation and output commands remain the controls, with the new
report renderer described in section 4.

Keep the requested spelling, but change dispatch deliberately: **`self optimize`
is project-pin-dispatched, unlike binary-management `self` commands.** Today
`selfmgmt::exempt_from_dispatch` exempts the entire namespace. A new ADR must
supersede that clause of ADR-0044 and make the exemption command-specific before
this entry point ships. Resolve the project selector before dispatch; capture,
execution and later `af task` inspection must use the same pinned release bytes.
An older project pin without optimizer support refuses and suggests an explicit
pin upgrade; it must not execute with the default binary. An old default binary
also needs an explicit update to recognize this exception. Fixtures cover both
version mismatches, `--repo`, and unchanged `self update/rollback` behavior.

Illustrative proposed entry point and existing control surfaces:

```sh
af self optimize --since 30d
# Project, history coverage, editable files, models/accounts, effects and budget.
# Task ID and exact PLAN_ID; no model invocation yet.
af task run TASK_ID --confirm-plan PLAN_ID
af task explain TASK_ID --tree
af task output TASK_ID --port report --format markdown --output optimization.md
```

`--since` filters by observed time, with an explicit UTC cutoff in the receipt.
Omitting it uses new evidence since the last completed capture, plus referenced
older evidence needed to interpret continuations. `--all-history` selects all
configured project history subject to limits. These are **collection windows**:
analysis aggregates the full retained project capture chain, including earlier
occurrences, under the captured retention policy. Its aggregate index records
every contributing capture ID and completeness; incremental reads never erase
repetition across runs. Detailed model input remains bounded. `--execute` is explicit automation
of the fixed outer Pipeline, consistent with `af task`; it does not approve
generated experimental plans or authorize adoption. No timer is installed.

The report leads with a concrete result, for example:

```text
Proposal: build the site gate in an isolated writable copy
Evidence: two failures caused by source-tree mutation
Validation: old gate fails both fixtures; candidate passes; acceptance preserved
Effect: verified correction; future token savings not yet measured
Changes: scripts/gate.sh and its captured check definition
Adoption: verified candidate available for local delivery; current project unchanged
```

The report separates `validated`, `rejected` and `recommendation_only` conclusions.
Those are domain conclusions, not replacements for Task execution/acceptance states.
An interrupted evaluator produces incomplete acceptance even if a candidate was built.
An unavailable required history source is incomplete, not “nothing to improve.”

Delivery is supported only for a verified configuration Snapshot, through the
existing explicit Task-ID delivery boundary into a new local worktree. No commits,
pushes or PRs are created by the component. Recommendation-only output can be
exported as a report, not delivered as an accepted code change. The owner reviews
and commits delivered configuration; only future Tasks capture that authority.

### Optimization strategies

`--strategy light|heavy` selects a versioned Pipeline package, not a hidden prompt
suffix. `light` is the routine default: one narrow proposal, stable Pipeline
structure, targeted cache/harness/context/Worker tuning. `heavy` permits redesign
of the complete project Pipeline closure, including decomposition, stage order,
routing, Workers and bounded concurrency, while preserving public contracts and
mandatory acceptance. Both reuse one Task runtime and the same independent oracle.
The strategy, effective package closure and search/resource bounds are captured
before execution and cannot change on resume.

Strategy is distinct from validation mode (`analyze|correct|measure`). Light may
measure a Worker tuning change; heavy can be run in analysis-only mode to propose
a redesign without executing it. Defaults are light with the project-selected
validation mode, and heavy with measured validation. Heavy without sufficient
case coverage, configured oracle or explicit resource allowance refuses execution
and offers an explicit analysis-only invocation; it never silently calls an
untested redesign verified.

Heavy's first version allows up to three development candidates and one selected
candidate evaluated on the hidden holdout. Development selection uses a captured
rule and evidence; candidate identity is frozen before opening the holdout.
The hidden set cannot be used to choose among alternatives. A negative result
ends that experiment; any later redesign retains its history and requires fresh
eligible holdout evidence. Generated definitions retain exact-plan approval.
Light's one-candidate rule is unchanged. Heavy is not permission for an unbounded
search, wider effects, a weaker verifier or automatic adoption.

## 3. What history is collected

The collector runs locally under configured source bindings. “All previous sessions”
means all available sessions explicitly associated with this project, not every
conversation accessible on the machine. Configuration enumerates adapters and
roots; the capture preview reports included, missing, excluded and unsupported
sources. New history adapters are explicit capabilities, not model-authored shell
commands that search the user's home directory.

Sources, in descending evidentiary strength:

1. AF Task/event prefixes, Attempt usage, context manifests, run diagnostics,
   verification receipts, Findings and delivery records.
2. Declared outer-session adapters for Claude/Codex JSONL: user requirements,
   visible assistant claims, tool calls/results, interruptions and manual repairs.
   Private reasoning/thinking blocks and unrelated conversation are excluded.
3. Captured Git snapshots/diffs and explicitly imported CI, deployment and client
   test receipts. No automatic remote access; adapters need their own binding.

AF records alone are insufficient: the website case's manual repairs and real
client failures occurred outside the AF Tasks. Conversely, an assistant saying
“verified” cannot override a failed check receipt. Correlation uses recorded Task,
Attempt, Snapshot, commit and invocation IDs. Repository provenance plus known
worktree relations establish project membership. A string mention of a path is a
discovery hint, not membership proof. Ambiguous associations remain unassigned and
are listed; an operator can explicitly import them with provenance.

Each source entry records adapter/version, project identity, session/execution ID,
bounded byte or event range, prefix digest, cutoff, observed timestamps, redaction
version and completeness. A running session is captured at a complete record
boundary; appends appear only in a later capture. Truncated records, rotated files,
unavailable blobs and conflicting IDs are explicit gaps. Prefix bytes are copied
or verified under a stable read before publication; a changing prefix is retried
within a bound or rejected. Resume uses captured bytes, never the live log.

Raw provider transcripts remain local in a restricted source store; models receive
only sanitized normalized artifacts and bounded excerpts. Headers, credentials,
environment dumps and known secret fields are removed before model input. Redaction
cannot promise to recognize every secret: unknown payload fields are withheld by
default, and projects opt in to additional fields. A withheld tool result remains
an observation of unavailable content, not a fabricated empty success. Retention,
deletion and source permissions follow machine-local policy; exported packages
contain no private logs. Missing retained evidence later makes a result
unverifiable, without deleting its recorded conclusion or manufacturing replay.

Deduplication operates on source identity and exact ranges, not prose similarity.
Native provider logs and AF receipts referring to the same invocation are joined,
not counted twice. Reports retain exact cumulative counter semantics from the Task
runtime. Unknown billing and censored unfinished runs remain unknown; summing
multiple cumulative report snapshots is forbidden.

## 4. Component architecture

The component is a small CLI/domain layer plus a shipped Pipeline package and
replaceable Workers. It reuses the Task compiler, scheduler, Store, Provider
admission, sandbox supervision and accounting. There is no optimizer daemon,
private scheduler, shell loop around `af task`, or second budget database.

```text
Project source + Optimization Policy + retained capture chain
                         |
        assign families + seal development/holdout split
                         |
          deterministic development-only profile
                         |
                diagnose Worker
                         |
           propose candidate(s) within strategy bounds
                         |
                 seal candidate
                         |
       compile + invariant + protected fixture checks
                         |
          experiment specification and admission
           |                                 |
   static repro sufficient          generated executable closure?
           |                          exact signed decision
           |                                 |
           +---------- bounded trials -------+
                              |
                      compare evidence
                              |
                 independent evaluate Worker
                              |
                   typed accept + report
```

The outer Pipeline is fixed and captured, with public required inputs
`source: af/SourceTree@1`, `history: af/OptimizationHistory@1`,
`policy: af/OptimizationPolicy@1` and Task Requirements. Proposed public outputs
are `report: af/OptimizationReport@1`, `result: af/OptimizationResult@1` and,
for candidate profiles, `snapshot: af/SourceTree@1`. Output lineage binds the exact
source, candidate, history, policy, experiment and evaluation identities.

There are two acceptance profiles selected at capture, not after seeing results:

| Profile | Required obligations and public evidence | Terminal behavior |
|---|---|---|
| Analysis | `analysis` on `result`, covered by an installed `OptimizationAnalysisReceipt@1` binding complete source checks and the selected independent `OptimizationEvaluation@1` | Evidence-grounded recommendations may be Satisfied; no Snapshot and no delivery. |
| Candidate (`correct` or `measure`) | `analysis` on `result` plus `verified` and `goal` on `snapshot`, covered by an installed `OptimizationVerification@1` binding the source/candidate, protected checks, comparison and independent evaluation | Positive verification emits the exact Snapshot and the `verified` domain conclusion; rejection or no eligible change is Unsatisfied and retains its report without a deliverable Snapshot. Missing required execution/evidence is Incomplete. |

The proposed `--mode analyze|correct|measure` selector makes this visible. The
project captures its default: `correct` when a protected oracle is configured,
otherwise `analyze`. There is no silent profile change once a Task revision exists.
A negative candidate result can still explain why no change should be adopted;
it cannot rewrite its fixed obligations into successful analysis. The terminal
result may omit a required Snapshot on failure, never on candidate acceptance.

All substantive analysis requires the independent evaluator.

Delivery needs an explicit optimize-profile adapter to the existing verified Task
delivery path; it checks `OptimizationVerification@1`, both code obligations and
exact output Snapshot identity. Merely naming a port `snapshot` is insufficient.
This adapter is part of M2 and preserves all existing delivery checks.

`OptimizationReport@1` is an installed pure rendering of the structured result and
its cited receipts, not `Document@1` and not independent acceptance evidence. Add
a type-dispatched Markdown renderer to `af task output`; the CLI example above
depends on that addition. This avoids changing the document acceptance contract
or pretending its checks and verifier ran. The renderer cannot upgrade conclusions.

Proposed intermediate contracts are `OptimizationProfile@1`,
`OptimizationHypothesis@1`, `OptimizationProposal@1`, `ExperimentSpecification@1`,
`ExperimentComparison@1` and `OptimizationEvaluation@1` in the `af/` namespace.
These need schemas, codecs, compiler coverage rules and validators; arbitrary JSON
from a Worker does not make them supported runtime artifacts.

Before diagnosis, the profiler assigns task families and seals the development
and holdout membership against the retained capture chain. `OptimizationProfile@1`
binds both set IDs but exposes only development-family aggregates and excerpts to
diagnosis and proposal authoring. Their retrieval capabilities also exclude
holdout history, prior holdout results and identifying case metadata. The policy's
selection procedure is fixed independently of candidate results. An experiment
must reference this already sealed holdout ID; its provenance must precede the
diagnose invocation. A late split or an exposed family is rejected as confirmatory
evidence. Report-only profiling has no hidden-test claim; families it exposes
cannot subsequently be relabeled as fresh holdouts.

The profiler computes deterministic counts before paying a model. Diagnosis sees
development aggregates and a small evidence index, retrieving bounded excerpts. The
proposer receives the selected hypothesis and relevant configuration closure.
The evaluator receives Requirements, protected policy, exact diff and evidence,
not the proposer's private transcript. It is independent of all candidate authors
under captured project policy. Report rendering consumes structured recorded
results; generated prose cannot upgrade their status.

Light permits one candidate per invocation; heavy permits its captured bounded
development candidate set. Neither automatically repairs after a negative holdout
experiment. This keeps selection bias and spend visible. A subsequent invocation
links the prior result and retains the failed candidate in the experiment history;
it cannot quietly reuse its holdout as fresh evidence.

## 5. What can change, and what remains authority

The Optimization Policy is committed project configuration, separate from the
candidate. It specifies exact allowed paths/packages and bounded tunables. Example
categories are Worker instructions/context recipes, compatible model/effort
bindings, sequential stages, retry feedback, check preparation and sandbox-local
dependency caches. File permission and symlink checks apply to the sealed diff.
Path permission is necessary but not sufficient: compiler and semantic checks
still validate the resulting closure.

Regardless of its allowlist, the author cannot write the Optimization Policy,
developer keys, machine-local source/binding policy, `.af/af.lock`, or any path in
the protected acceptance closure. Capture that closure, including its transitive
scripts and fixtures, from trusted authority into a Store artifact before
authoring; pin its digest in the outer plan and materialize it read-only, outside
the candidate mount. If a target harness file is also in the protected closure,
that proposal is ineligible; a separate authority change must establish a suitable
independent oracle first. Ordinary target harness files are not all prohibited.

Mandatory goal acceptance, required checks, verification independence, retention,
evidence lineage, effect ceilings, original Task allowance and generated-plan
approval are protected. The optimizer cannot gain savings by deleting tests,
skipping reviewers required by policy, accepting incomplete results, increasing
its own allowance or changing the pass threshold. A tuned operational timeout or
reservation may change only inside pre-authorized ranges and within the original
total limits; relaxing a mandatory policy limit is recommendation-only.

The candidate may edit a project's gate implementation only if an independent,
captured acceptance harness tests the intended semantics. That harness is stored
outside candidate-writable files and includes its transitive scripts, fixtures and
oracle. A pinned shell wrapper that calls a candidate-controlled `npm test` is not
by itself an independent acceptance harness. If no such oracle exists, v1 reports
the proposal and the missing fixture instead of claiming validated improvement.

Package digests and locks are produced by AF's existing canonical tooling on exact
candidate bytes, never hand-generated in a model response. A trusted finalizer
may update only the package-pin entries entailed by the admitted patch, while
preserving engine release pins, protected-policy entries and unrelated bytes.
This is the sole exception to the author's lock-file prohibition. Its diff and
resulting closure are independently checked and included in the final sealed
Snapshot before evaluation; a later repin cannot inherit an earlier verdict.
Shared upstream
packages are not overwritten: a proposal creates a project-local package or an
explicit shared-catalog change for later owner-controlled publication. Provider
account labels may be suggested only from allowed bindings; credentials and
machine-global settings are never in the patch.

## 6. Experiments and generated-plan approval

Historical analysis nominates a hypothesis; it does not prove causality. The
Experiment Specification fixes the baseline/candidate closures, selected cases,
environment and cache state, oracle, required outcomes, sample/repetition limits,
resource caps and success rule before candidate trial results are available.
Actual runs are new executions of captured inputs, not deterministic replay of
old model behavior. Tool/network effects are fixture-backed; production writes,
publication and credential acquisition are absent.

For measured configuration trials, the case's captured product source Snapshot
and Requirements stay byte-identical in both arms. Resolve execution packages and
Worker bindings separately, from the exact baseline or candidate configuration
closure, and record that authority independently of the case source. Do not load
the case tree's `.af/` implicitly or overlay files into it while retaining its old
Snapshot identity. A family is eligible only when its historical baseline
execution package matches the selected baseline package and both arms' interfaces
fit the original Requirements and oracle; exclusions are listed before trials.
The compiler must support and verify this explicit source/authority separation
for experimental Tasks. Until then such cases are recommendation-only.

Harness-file changes use the deterministic-correction route in v1: an installed
fixture constructor materializes the fixed fixture tree with either the baseline
or candidate target harness bytes. Each materialization is a distinct derived
Snapshot, bound to the fixture constructor, exact harness artifact, environment
and protected oracle. No third tree is passed off as a historical Snapshot. A
mixed package-and-harness proposal is outside v1; split it into separately
evidenced changes. Production feature changes are never smuggled into the fixture.

Two validation levels are explicit:

| Level | What it establishes |
|---|---|
| Deterministic correction | A recorded failure is reproduced under baseline and resolved under candidate, while protected regression fixtures and contracts pass. Does not establish model-cost or task-success improvement. |
| Measured optimization | Matched baseline/candidate trials under the same protected acceptance oracle meet the predeclared objective without prohibited regression. Uncertainty and per-case failures are reported. |

For model/effort/context/routing changes, static compilation is insufficient:
measured optimization is required for a validated conclusion. Report-only mode
may still recommend them when trial budget or evaluable cases are unavailable.
For a gate path correction, deterministic reproduction may be sufficient.

Candidate Pipelines or changed Worker execution definitions are untrusted proposed
authority. Compiling them to run trials requires a captured experimental execution
closure and an authenticated exact-plan developer decision. The fixed outer
Pipeline's confirmation, a model verdict and `--execute` cannot supply that
decision. This applies to generated definitions at every embedded depth. Merely
parsing TOML or inspecting a diff executes none of those definitions and needs no
generated-plan execution approval.

AF needs an explicit admission barrier for experimental child closures produced
mid-Pipeline. This requires a new accepted ADR extending ADR-0046/0056/0081,
versioned execution/decision contracts and compiler/Store checks. It is not a
permissive interpretation of today's data-only owned Review children.

The outer plan captures an **experimental slot** with fixed public interface,
allowed Task kinds/packages/effects, maximum child count/depth, source mapping,
protected oracle and shared resource ceiling. It grants preparation authority,
not permission to execute a model-produced definition. Its digest is part of the
initial outer plan. The proposed protected transition is:

```text
captured bounded slot
    -> ExperimentPrepared(specification, exact compiled child closure)
    -> needs_plan_review (no generated child reservation or dispatch)
    -> authenticated ExperimentPlanDecision
    -> ExperimentChildrenRegistered
    -> ordinary shared scheduler -> comparison -> acceptance
```

`ExperimentPrepared@1` is written under the current Task writer lease and binds
the Task revision, outer plan ID, slot ID, specification ID, compiled child-plan
ID, policy ID, case-set IDs and spent-accounting prefix. The compiler checks the
complete child closure against the slot's immutable constraints and remaining
original allowance. The specification names both trial arms and every repetition.
Its child-plan identity includes all effective Worker bytes, bindings, inputs,
effects and nested generated definitions. Any changed dependency produces a new
prepared identity, never a mutable replacement of the captured artifact.

`ExperimentPlanDecision@1` is a new signed payload, not reuse of an ordinary
PlanDecision with extra fields. It binds that full prepared identity, developer,
decision, expiry and key-policy identity. Registration rechecks the signature,
revocation, writer epoch, exact dependencies and remaining allowance atomically.
Only the admitted slot can register children, and the registered set is immutable.
The original outer plan and its confirmation remain valid for the fixed outer
work; they do not confer authority over the separately approved child closure.
Changes outside the captured slot require a new outer plan and its normal
approval. The first measured increment permits one prepared experimental closure,
with no automatic repair
or replacement after rejection. Versioned inspection shows outer and child IDs
separately rather than presenting the outer ID as the complete dynamic authority.

Each child has an ownership edge and spends Attempts, tokens, wall time and
concurrency under both its registered sub-allocation and the original parent
limits. Starting an independent CLI Task to evade limits is forbidden. Rejected,
expired or revoked approval dispatches no generated child. Waiting consumes the
original deadline. Successful children survive resume; a changed or revoked
decision cannot authorize recovery that introduces new work. Historical Task
and owned-Review-child wire generations retain their original behavior.

Changed instructions, context recipes, models and effort are **execution
authority**, even if transported as JSON. This design deliberately does not
remove their approval requirement by calling them data. Deterministic fixture
inputs can vary without generated-plan approval only when an already captured
trial operator executes them under unchanged package, binding and effect authority.
The installed context recipe therefore uses its typed configuration only as a
kernel derivation proof: AF derives a content-addressed Worker package that changes
only `instructions.md`, places that package identity in the separately approved
candidate closure and makes the common host render native context from it. The
derivation artifact is not an arm input. Registration revalidates the original
package, candidate source, repin, exact changed bytes and unchanged manifest,
runner, contract and remaining package files. Ordinary execution after adoption
loads the same repinned package and consequently the same instruction identity.

A baseline captured in history is not automatically authorized to execute today.
Both baseline and candidate receive current binding/admission checks. A baseline
that cannot run makes the comparison inconclusive unless the declared
deterministic fixture explicitly tests that refusal as the defect. Missing
dependencies cannot be silently substituted. Restart restores the captured case
set and selected outcomes; successful child Attempts are not repeated on resume.

## 7. Objective and resistance to overfitting

Apply AF's engineering values: preserve correctness and mandatory acceptance,
then optimize total tokens per verified outcome; context, wall time and operator
interventions are visible secondary measures. No single weighted score may hide a
correctness regression behind cost savings.

The baseline profile groups compatible Task kinds and acceptance regimes. Report
failure/retry rates, all spent tokens including failures and admission, context
sizes, wall time, and intervention observations separately. Latency claims specify
whether queueing, user waiting and cache warmup are included. Incomplete native
billing prevents an exact savings claim. Intervention counts inferred from outer
logs are labeled observed, not a complete measurement of human effort.

For measured optimization, cases are frozen before proposal authoring. Split at
the underlying task family, including retries and near-duplicate inputs, so a
repair does not leak into the holdout under a different session ID. Proposal
authors see the development set; only protected evaluators execute the holdout.
V1 has no automatic statistical claim from a handful of cases: the policy fixes
minimum sample size, paired repetitions, tolerance and confidence method; below
that threshold, results remain exploratory/recommendation-only. Exact
deterministic corrections use a separate explicit acceptance rule rather than
pretending to have a large sample.

Repeated optimizer runs retain a local experiment registry keyed by case-family
identities and candidate lineage. A holdout exposed by a previous report is marked
used and cannot support a fresh confirmatory claim. Stable baseline task families,
sample selection and every failed candidate remain visible. Future observation
after owner adoption is observational evidence, stratified by model/engine/
environment versions, not proof of causality across changed workloads.

The report includes optimization cost and estimated break-even only when savings
and anticipated comparable task volume are available. A zero or negative saving,
unknown usage, changed acceptance regime or unsupported estimate is stated as
such.

### Token and time economics

The optimizer must have enough evidence to explain spending and to execute useful
cache, harness and tuning changes. Add a typed `OptimizationEconomics@1` artifact,
consumed by diagnosis, experiment selection and independent evaluation. It keeps
native measurements, AF's accounting and derived estimates distinct:

| Dimension | Required observations where available |
|---|---|
| Tokens | Native input/output, cache read/write, reasoning, AF chargeable total, context manifests, retrieval volume, retries/admission/failed and abandoned work, measurement completeness. |
| Time | Task elapsed time, provider and gate duration, queueing, environment/dependency preparation, retrieval, execution, verification, approval waiting and observed user intervention; overlap and clock provenance. |
| Cache | Cache kind, eligibility, hit/miss/unknown, bytes/tokens reused, key/invalidation identity, population and lookup cost, storage/copy overhead and cold/warm state. |
| Outcome | Exact Requirements and verifier identity, accepted/failed/incomplete outcome, repairs and later defects/client results when captured. |
| Attribution | Project, case family, Task/Attempt/invocation, node, model/effort, package/plan and environment identities, source receipt and observation cutoff. |

Missing historical fields remain unknown and lower confidence. New AF executions
instrument missing spans at shared runtime/supervisor boundaries; adapters must
not invent provider-internal timings. Duration sums are not user elapsed time when
work overlaps. Report active work, observed waiting and end-to-end elapsed time
separately; derive a critical path only from supported dependencies and spans.
Reasoning may be included in output, and cached input may be included in input:
retain native semantics and never sum overlapping dimensions as independent spend.
AF chargeable tokens bound execution; they are not a provider invoice or a universal
cross-provider monetary measure. Optional money estimates name their captured rate
source/date and native billing interpretation; no prices are hard-coded here.

The objective is total resource use per verified outcome, subject to unchanged
correctness and explicit latency limits. Every comparison includes failures,
admission, preparation, evaluation, cache warming and the optimizer's own analysis
and experiments. Report both per-Task and cohort totals with their denominators;
zero verified outcomes means undefined cost per verified outcome, never zero cost.
Token and time improvements are separate claims; a time-only improvement may be
accepted only under a captured latency objective and token-increase ceiling.
Otherwise token economics remains the default priority.

Supported strategy families include provider-supported prompt-prefix reuse,
content-addressed retrieval/context reuse, sandbox-local dependency/build caches,
exact deterministic artifact reuse, check preparation, retry feedback, compatible
model/effort tuning, and heavy structural redesign. Every recipe declares when it
applies, which evidence it needs, candidate effects, invalidation/acceptance rules
and how to measure the benefit. The optimizer authors and tests the change when
those requirements are met; it does not stop at generic caching advice. Unsupported
provider capabilities or required kernel changes become explicit upstream work.

Cache keys bind the relevant source/content, input, configuration, toolchain and
policy identities. Validation and execution authority are rechecked when required;
never cache away approval, revocation, evidence integrity or current acceptance.
Sandbox-local caches preserve isolation; a proposed host-cache bypass is rejected.
The installed Cargo hook is selected only by the captured source file
`.af/cache/cargo.json` with the exact closed payload
`{"schema":"af.sandbox-cache-selection/1","kind":"cargo"}` and an
administrator-admitted cache mapping. The shared code-Task environment consumes
that selection for candidate trials and later ordinary non-writing command checks;
model and source-writing Workers do not inherit command-only cache variables. The
runtime receipt retains actual cache-source digest separately from the combined
invalidation identity, and a captured toolchain declaration or an explicit missing
measurement rather than substituting the AF engine identity.
Prior model answers or success verdicts are not interchangeable with deterministic
artifacts. Provider cache hits are observations, not a guarantee from prompt shape.
Measure cold and warm behavior under the same declared population/warmup rules,
including cache-write and preparation overhead. A cache change that makes tests
stale or leaks data fails even if its token count falls.

For each candidate, report gross expected savings, optimization/validation and
cache setup costs, steady-state overhead, expected comparable future volume and
break-even where estimable. `break_even = ceil(one_off_cost / positive_net_saving)`
is computed independently in compatible token, time or money units. Unknown or
nonpositive savings have no finite supported break-even. Spend forecasts are
hypotheses; adoption reports show observed results and version/workload changes.
A candidate whose expected useful lifetime cannot repay validation cost should be
left as a recommendation unless a captured correctness/latency objective justifies
it. Light uses cached deterministic profiles and narrow retrieval to keep its own
routine cost low; unchanged captured inputs can return the recorded result.

## 8. Failure, state and repeatability

History Capture and experiment artifacts live in the normal Store, not `.af/`.
The experiment registry is a rebuildable projection of Task events/artifacts,
not separate coordination state. Sensitive raw-source storage is machine-local
and access-controlled; normalized retained artifacts have the existing Store's
identity and retention semantics. Redaction does not grant authority to log text.

An optimization execution key includes project identity, history digest, source
Snapshot, policy, optimizer closure and environment identity. Repeating the same
request can return its completed result with zero new inference; a running Task
is resumed by ID. The command must not silently return a previous result when
sources or authority changed. New source capture records the old cursor and new
prefixes; a failed run does not make those observations disappear from discovery.

Malformed history produces typed diagnostics. Missing evidence for one hypothesis
does not contaminate unrelated supported observations, but all exclusions appear
in the coverage report. Budget exhaustion preserves candidates and receipts and
returns incomplete acceptance. A failed experiment is retained negative evidence,
not retried until green. A stale delivery target refuses under the existing exact
source/clean target rule. Previously admitted project Tasks continue using their
original configuration after adoption.

## 9. Readiness and implementation boundaries

| Capability | Current foundation at `166eca5` | Required addition |
|---|---|---|
| Execute optimizer as Pipeline | Common Task runtime, typed ports and native Workers | Optimize Task-kind package, contracts and public coverage |
| Collect project history | Durable Task logs, usage and diagnostics; source capture patterns | Project-scoped index, versioned session adapters, immutable prefix capture and redaction |
| Diagnose cheaply | Exact context manifests, bounded retrieval patterns | Deterministic profiler, observation taxonomy and role-specific inputs |
| Author configuration | Implementation sandbox/seal, local bindings, catalog export | Constrained configuration proposal plus canonical repinning |
| Preserve acceptance | Captured authority, independent goal verification | Protected optimization oracle and transitive harness capture |
| Test candidate plans | Generated-plan approval; owned Review child implementation exists but ADR-0081 remains Proposed | Accepted experimental-slot ADR, new signed child decision and shared accounting; not assumed shipped |
| Report and deliver | Typed artifact output and verified Task delivery | Optimization report renderer, profile-specific acceptance receipts and delivery adapter |
| Learn across runs | Store/event projection machinery | Case-family/exposure registry, capture cursor and adoption observations |

Place domain contracts under `review-core/src/task/optimization`, configuration
under `review-config/src/task`, and orchestration entry points in a separate
`af/src/self_optimizer` module delegating to `task_execution`. History
adapters should have a dependency-neutral module/crate boundary with no Provider
credentials or execution control. Generic child admission belongs in the common
runtime, not in `selfmgmt.rs` or a special optimizer executor. The shipped Pipeline
and Workers use the same catalog packaging and version/digest rules as other
Task kinds. CLI help must distinguish project optimization from `self update`.

## 10. Implementation milestones and exit criteria

The architecture is delivered as four large, dependency-ordered milestones. Each
one states the product capability it adds and the evidence that closes it; no
milestone is complete until that evidence exists.

| Milestone | Product capability | Defining exit evidence |
|---|---|---|
| M1: Trustworthy project economics | Captured history, exact token/time attribution, cache observations and report-only Pipeline | Reconciled AF totals, separate outer-session costs, overlap-aware timing, retained recurrence and explicit unknowns. |
| M2: Controlled experiments | Protected oracle, candidate/analysis profiles, signed experimental slots, shared accounting and verified delivery | Accept a correct candidate, reject unsafe/cheaper-but-worse alternatives, and resume without repeating successful paid work. |
| M3: Light optimizer | One concrete routine cache/harness/Worker change from diagnosis through adoption evidence | Demonstrated token-saving and time-saving changes with acceptance preserved and optimizer overhead included. |
| M4: Heavy redesign | Whole-Pipeline candidates, bounded development selection, independent holdout and longitudinal economics | A structural redesign with measured benefit, adoption observations and explicit regression/rollback evidence. |

M1 is useful for diagnosis; M3 is the first end-to-end self-improvement release.
The full requested light/heavy product completes at M4. No stage may advertise
measured improvement merely because its schemas, compilation or UI pass.

ADR-0104 accepts exact-plan preview, ADR-0105 accepts project-pin self-optimization dispatch,
ADR-0081 accepts common-runtime owned children, and ADR-0106 accepts the signed experimental-slot
extension. Code and credential-free fixtures remain implementation evidence; the separate live
milestone demonstrations and release gates supply adoption evidence.

Required credential-free fixtures cover duplicate cumulative usage, unknown
billing, overlapping timing spans, partial records, worktree misassociation,
prompt injection/redaction, cache invalidation, stale source/policy, modified
protected oracles, holdout leakage, unsigned generated children, child budget
escape, negative/missing evaluation, no-evidence replay, and undeliverable results.
Every implemented milestone receives the applicable full repository checks and
external AF review before its exit criteria are marked complete. Live dogfood
records actual model/effort, per-Attempt usage, elapsed time and observed outcomes;
synthetic fixtures alone establish no savings claim.

## 11. Worked website example

The September 15–16 website rollout is a motivating case, not a portable training
corpus. A local import links AF receipts to the owner's outer-session records.

| Observation | Appropriate optimizer response |
|---|---|
| Build writes into AF's read-only check tree | Propose isolated writable build copy; reproduce under the exact check invocation. |
| Inlining gate into `bash -c` makes `BASH_SOURCE` empty | Add source-root guard and fixture for both supported invocation forms. |
| Verification-only retry runs an unchanged implementer | Recommend a supported verification route; if runtime support is missing, produce an upstream request rather than inventing an operator. |
| Claude client fails although MCP contract tests passed | Recommend a protected client-journey oracle asserting actual MCP tool calls; do not patch product protocol code in an optimization Task. |
| Healthy data build ID hides the old running binary | Recommend executable-identity acceptance in deployment harness; fixture-backed harness repair only, no production mutation. |
| Smaller model appears cheaper on successful Tasks alone | Include failed attempts and acceptance outcomes; withhold savings claim without matched trials. |

The durable learning is a versioned, tested project definition plus its evidence.
It is not an ever-growing prompt of lessons copied into every Worker.

## 12. Alternatives and decisions for review

- **A script that reads every transcript and rewrites `.af/`:** rejected; it loses
  evidence identities, resource accounting and the separation of data/authority.
- **Only AF event logs:** rejected; outer-session repairs and user outcomes are
  central evidence in the motivating case. They remain opt-in scoped sources.
- **Automatic activation after a model approves:** rejected; generated execution
  and new project authority need their existing developer/owner boundaries.
- **Treat static contract checks as optimization proof:** rejected; they establish
  compatibility, not outcome quality or savings.
- **Full reinforcement learning/global policy training in v1:** deferred; a
  project-local proposal and controlled comparison provide useful value first.

Defaults proposed for the routine strategy are one candidate, manually invoked capture, report-first
preview, explicit local delivery, no automatic activation, and no global agent or
AF binary edits. Initial shipped limits, captured in every plan, are:

| Limit | Proposed default |
|---|---:|
| Newly selected sessions per capture | 200 |
| Raw bytes read per capture | 256 MiB |
| Maximum single record | 1 MiB |
| Normalized retained text per capture | 16 MiB |
| Initial input per model Worker | 24 KiB |
| Additional retrieved text per model Worker | 96 KiB |
| Analysis/deterministic-correction parent allowance | 400,000 chargeable tokens / 12 Attempts / 60 minutes |
| Measured-mode parent allowance | Explicit owner-selected total, at least fixed outer bounds plus all admitted trial bounds; see formula below |
| Protected final verification reserve | 100,000 tokens / 3 Attempts / 15 minutes |
| Proposed candidates | Light: 1; heavy: at most 3 development candidates, then 1 selected holdout candidate |

Hitting a capture limit produces a coverage gap and continuation cursor; it never
silently truncates “all history.” A run with incomplete required coverage may
produce only an explicitly partial report.
Parsing is streaming and record-bounded; a giant or malformed record cannot force
an unbounded allocation. Raw and normalized byte limits are separate from model
context limits. Provider admissions and failed Attempts spend the outer allowance.

The default allowance is for light analysis/deterministic correction. It does
**not** support heavy redesign or the reference measured comparison. Heavy must
add every authoring/selection Attempt and every development arm to its initial
parent allowance; the same shared-accounting formula applies.
Measured trials are opt-in with a separately declared allocation *within* an
explicitly larger original parent allowance, not an extra budget. At capture,
the frozen case count and captured trial-interface bounds determine the required
envelope before diagnosis or authoring:

```text
minimum parent Attempts = fixed outer Attempts
                        + sum(all baseline/candidate repetition Attempt bounds)
minimum parent tokens   = fixed outer reservations
                        + sum(all baseline/candidate repetition token bounds)
```

Each bound includes required child checks/evaluators and provider admissions not
already shared under the captured policy. The outer verification reserve remains
protected throughout; it is part of the fixed outer allocation, not free extra
capacity. The plan also checks the captured concurrency and wall-time envelope.
The prepared exact child closure may tighten but never enlarge the approved
envelope. If it cannot fit, the candidate Task remains incomplete with its
recommendation report; it does not silently change profile or reset its deadline.

Twenty families, two repetitions and two arms mean 80 trial executions. If each
trial requires five Attempts including its checks/admission, the total is at
least `12 + 80 * 5 = 412` Attempts, with independently sufficient token/time limits.
That is an arithmetic example, not a promise that five Attempts cover every Task
kind. `--mode measure` refuses insufficient limits before authoring and displays
the needed envelope and an explicit `--mode analyze` alternative. A
reference measured token-economy policy requires at least 20 independent case families, two
paired repetitions, zero new protected correctness failures, no lower verified
completion count, and a positive lower bound on token savings under a predeclared
95% paired family-bootstrap interval (10,000 deterministic-seed resamples). The
comparison includes all failed-run spend. This is a conservative eligibility rule,
not a universal statistical guarantee; projects may require a stronger policy.
Lower sample sizes remain exploratory. Large trials need an explicitly larger
parent allowance at initial capture and should justify their break-even estimate.
