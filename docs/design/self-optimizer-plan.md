# Self-optimizer implementation plan

Status: implementation in progress, 2026-09-17. M1 and M2 code and credential-free fixtures are
implemented. M3 now has an executable light diagnose/propose/protected-experiment path and typed
recipe, economics and adoption contracts. Live paid demonstrations, persisted post-delivery
adoption observations and the external milestone release gates remain pending. M4 has not started.
This extends the [design](self-optimizer.md) with the owner's requirements for
token/time economics, routine optimization and complete Pipeline redesign.
These additions postdate the Fable 5.1 design review; that review does not cover
this plan or the new strategy behavior.

The [M1–M3 Task Pipeline plan](self-optimizer-task-pipelines.md) specifies how to
execute the first three milestones, including Worker roles, stage dependencies,
acceptance evidence and resource planning.

## Product outcome

After working in a project, the owner runs a normal AF Pipeline that examines the
history, identifies expensive or unreliable behavior, makes a concrete change,
validates it and produces a reviewable configuration Snapshot. It reports how
much was spent and saved, including the optimization itself. Generic caching
advice is insufficient when a supported change can be implemented and tested.

Proposed interface:

```sh
af self optimize                         # light, captured project defaults
af self optimize --strategy light        # routine upkeep
af self optimize --strategy heavy        # bounded complete Pipeline redesign
af self optimize --strategy heavy --mode analyze
```

These commands initially capture and preview. Existing Task controls execute,
inspect, resume, cancel, export or deliver. Strategy controls search scope;
`analyze|correct|measure` controls acceptance. There is no second scheduler,
private accounting service or automatic activation.

| Strategy | Light, routine default | Heavy, explicit |
|---|---|---|
| Question | Where can this working configuration waste less? | Is this the right Pipeline structure for this project? |
| Scope | One narrow cache, harness, context, retry or compatible Worker tuning change; stable topology | Complete Pipeline closure: decomposition, stage order, routing, Worker roles/models, concurrency and evidence handoffs |
| Search | One candidate; no open-ended repair loop | At most three development candidates; one selected for confirmatory holdout evaluation |
| Evidence | Incremental collection with retained trends and targeted development evidence | Representative families, failure modes, dependencies and development/holdout partition |
| Cost | Cheap profiling first; bounded search and separately budgeted paid trials | Explicit larger envelope covering all alternatives, trials and final verification |
| Shared rules | Required acceptance, isolation, generated-plan approval and final local delivery | The same rules; heavy does not weaken them |

## Milestone sequence

```text
M1 — Trustworthy project economics
              ↓
M2 — Controlled optimization experiments
              ↓
M3 — Light optimizer: first complete improvement loop
              ↓
M4 — Heavy redesign and benefit after adoption
```

Each milestone is a coherent capability with an end-to-end demonstration. It may
span multiple implementation PRs, but is complete only when its integrated exit
criteria pass. M1 is useful for diagnosis; M3 is the first end-to-end improvement
release; M4 delivers the full requested light/heavy product.

## M1 — Trustworthy project economics

**User-visible result:** AF explains where project tokens and time went, which
costs repeat, and which evidence is missing. Its analysis is reproducible from
captured inputs and runs through the common Task runtime.

Build the project-scoped history collector, normalized observations, retained
capture-chain index and typed `OptimizationEconomics` projection. Correlate AF
events, declared session logs and explicit external outcome receipts by exact
execution/source identities. Include failed, cancelled, abandoned and resumed
work; deduplicate overlapping logs and cumulative usage. Exclude private reasoning
and sanitize context before model access. Missing measurements remain unknown.

| Dimension | Minimum useful detail |
|---|---|
| Tokens | Native input/output, cache read/write, reasoning where reported, AF chargeable totals, context/retrieval volume and repeated context; preserve overlapping-counter semantics |
| Attribution | Task family, Pipeline/node/Worker, model/effort, outcome, configuration/environment versions; identify inner/outer execution overlap |
| Time | End-to-end elapsed, active work, checks, dependency preparation, queue and approval/user waiting where observed; do not sum overlapping spans as elapsed |
| Cache | Type, eligibility and observed hit/miss/unknown, warmup/lookup costs, invalidation identity, bytes/tokens reused, cold/warm state |
| Outcome | Requirements/verifier identity, verified completion, failed/incomplete status, retries, repairs and captured later defects |
| Confidence | Source receipt/range, cutoff, exact/estimated/lower-bound/unknown status and missing fields |

Reuse existing exact usage types and the common ledger. Add missing lifecycle
and cache spans at shared runtime/supervisor boundaries; do not invent provider
internal timing. Historical adapters retain unknown spans. Money estimates are
optional and name their captured billing/rate source; chargeable tokens are not
relabeled as dollars. Unknown attribution cannot support exact per-component claims.

Also deliver project-pin dispatch for `self optimize`, configuration and strategy
declarations, a report-only Pipeline, typed report rendering and plan preview.
Resolve dispatch/preview ADR prerequisites here. Record case-family exposure from
the first report so later holdouts cannot reuse disclosed evidence.

**Exit demonstration:** import the website rollout and a second project or
synthetic concurrent workload. Reconcile AF totals against receipts; show outer
session costs separately without double counting; identify harness retries and
distinguish elapsed time from summed work. Append logs without recounting earlier
usage, preserve repeated failures across captures, and replay unchanged captured
analysis without fresh inference. Missing costs/timings are visibly unknown.

**Primary areas:** `review-core` optimization/usage contracts, Store projections,
session adapters, shared timing instrumentation, CLI dispatch/output and catalog
packaging. No autonomous configuration edits yet.

## M2 — Controlled optimization experiments

**User-visible result:** given a concrete candidate, AF can explain and run a
bounded baseline/candidate comparison, then accept, reject or retain an
inconclusive result with exact evidence. Autonomous proposal generation is not
required to demonstrate this milestone.

Build the protected Optimization Policy and transitive acceptance harness,
configuration diff validation, trusted package repinning and separate analysis
and candidate profiles. Freeze case families and development/holdout membership
before diagnosis. Preserve source identity while binding baseline and candidate
execution authority separately. Harness tests use explicitly derived fixture
Snapshots rather than silently modifying historical inputs.

Implement the experimental-slot prepare/decision/registration transitions from
design section 6. This is the principal runtime extension: generated child plans
need exact signed authority, while all children consume the same original parent
budget, deadline, concurrency limits and recovery log. Bound future multiple
candidate development in the slot, with one selected confirmatory candidate.
Never rewrite the outer DAG or start independent CLI Tasks to obtain fresh budgets.

Deliver comparison recipes for deterministic correction, token savings and
latency improvements. Each captures its metric semantics, quality requirements,
case selection, repetitions and uncertainty rule before results. Default priority
is tokens per verified outcome with correctness and latency constraints; time-only
optimization needs an explicit objective and token ceiling. Zero successes means
undefined cost per verified outcome. Cheap deterministic fixtures or exploratory
trials cannot support a broad model-performance claim.

Measure cold/warm cache behavior fairly, including population, lookup and copy
costs. Cache reuse cannot skip current authority or evidence-integrity checks.
Include admission, failed arms and retries. Initial budgets sum every arm,
repetition and required-verifier bound: the 12-Attempt light default cannot stand
in for the 80-execution reference model comparison.

Provide the optimize-profile adapter for verified local delivery. Negative or
report-only results cannot enter it. Independent evaluation consumes the final
candidate and comparison evidence, not the author's private transcript.

**Exit demonstration:** accept a correct harness repair; reject candidates that
delete checks, widen cache access, reuse stale results or reduce verified
completion to appear cheaper. Compare matched arms and charge every invocation
once. Interrupt and resume across approval, child settlement and publication
without repeating successful paid work. Stale approval, insufficient samples,
incompatible cases and exhausted resources produce explicit non-success results.
Use credential-free fixtures with genuinely sufficient declared limits.

**Primary areas:** common compiler/Store/scheduler, versioned experimental
authority, acceptance, sandboxed fixtures, comparison operators and delivery.
Accept the architectural extensions before enabling new dispatch.

## M3 — Light optimizer: first complete improvement loop

**User-visible result:** `af self optimize --strategy light` finds a worthwhile
routine improvement, writes it, tests it and offers verified local delivery with
before/after economics. The owner does not manually translate advice into code.

Ship the light Pipeline, role-scoped diagnostic/proposal/evaluation Workers and
an initial recipe catalog. Recipes declare applicability, required observations,
writable effects, validation, invalidation and expected payoff. They must execute
supported contracts; prose-only suggestions are not installed recipes.

Initial recipes cover:

- Provider-supported prompt-prefix reuse, exact retrieval reuse and reduced
  duplicate injected context.
- Sandbox-local dependency/build caches and economical gate preparation, with
  source/toolchain/policy-aware invalidation and cold/warm measurements.
- Exact deterministic artifact reuse under current integrity and authority checks,
  preserving mandatory verification instead of caching a success verdict.
- Targeted retry feedback and compatible Worker model/effort/context tuning under
  unchanged topology and acceptance, using available runtime capabilities.

Select a concrete hypothesis using avoidable cost, validation expense and expected
comparable future workload. A stable-looking prompt prefix does not prove a cache
hit. Unsupported capabilities become explicit upstream work, not invented
operators or global configuration edits. Light executes only one candidate and
never repairs repeatedly until a held-out test passes.

Instrument the optimizer's own analysis, proposal, trials, evaluation and setup.
Report gross savings, recurring overhead, one-off cost and break-even separately
for tokens and time. Do not spend a large experiment budget to save a negligible
amount on infrequent work. State explicitly when correctness or latency justifies
a negative token/cost payoff.

Add adoption receipts linking delivered configuration to the actual later commit,
without making the optimizer commit it. Edits during adoption invalidate candidate
equivalence. Begin observational follow-up here; workload/version differences
remain visible and are not presented as causal proof.

**Exit demonstration:** a live project receives an end-to-end token-saving change
and a time-saving cache/harness change with unchanged required acceptance. Record
actual evidence, including optimizer overhead and unsuccessful candidates; impose
no invented improvement percentage. Withhold negative-payoff or insufficient-data
candidates as recommendations. Replay unchanged analysis without inference. Verify
cache invalidation and adoption identity, then observe subsequent real Tasks.
If useful savings are not demonstrated, the economics exit criterion remains
open even when the CLI works.

**Primary areas:** shipped Pipeline/Worker packages, recipe contracts, context and
cache hooks, economics selection, reporting and adoption projection. M3 is the
first complete self-improvement release.

## M4 — Heavy redesign and benefit after adoption

**User-visible result:** `af self optimize --strategy heavy` can replace the
complete project Pipeline structure with a better validated design, then show
whether adopting it helped in real work.

Ship a distinct heavy Pipeline package sharing the same history, experiments and
acceptance machinery. Build an execution map from dependencies, failure
propagation, repeated context, supported critical-path spans and interventions.
Consider changes local tuning cannot make: move checks earlier, combine redundant
nonmandatory work, change decomposition/routing, reassign Worker responsibilities,
improve evidence handoffs and change bounded parallelism. Preserve public
contracts and mandatory acceptance; removing a mandated reviewer is ineligible.

Generate at most three coherent full-closure candidates under captured bounds.
Compile and obtain exact approval for generated execution, run development trials,
retain every rejected alternative and its cost, and select one with the captured
rule. Freeze it before holdout evaluation. Hidden cases never choose the winner;
a negative holdout ends the run. Nested experiments cannot multiply budgets.
Measure planning, coordination, context and verification costs as well as Workers.

Complete the adoption loop: compare later observed cohorts, flag workload/version
changes, track whether savings repaid optimization cost, and surface regressions.
A rollback is a new reviewable configuration proposal against current source,
not a silent overwrite. Exposed holdouts stay used across strategies and sessions.
The optimizer may propose its own future package changes only under the same
fixed-current-authority rule.

**Exit demonstration:** redesign a real Pipeline with a material topology change,
not merely a model swap. Show baseline, alternatives, selection, held-out outcomes,
total optimization spend and break-even. Deliver the accepted candidate and
observe subsequent executions, distinguishing controlled evidence from observed
benefit. Exercise negative holdout, resume, stale adoption and proposed rollback.
If no redesign improves the captured objective, report rejection honestly and
keep the positive-value dogfood criterion open.

**Primary areas:** heavy Pipeline/Workers, whole-closure authoring/selection,
expanded contract fixtures, longitudinal economics and adoption/rollback evidence.

## Completion and scope discipline

Each implemented milestone requires focused tests, compatibility fixtures, the
applicable repository `make check`, and a light external `af review` under the
effective project review policy, followed by concrete fixes and deterministic
checks. Record actual model/effort, wall time, per-Attempt usage and finding
dispositions. Heavy optimizer strategy does not imply a heavy convergence review
Campaign; those are distinct settings.

No arbitrary calendar estimates or ticket explosion are needed yet. M2 is the
largest architectural risk and intentionally precedes autonomous tuning/redesign.
Build history, experiments and acceptance once for both strategies. Each milestone
states supported capabilities, instrumentation gaps and exactly what it proved.
A report-only release does not complete self-improvement; the full requested
product ends at M4.
