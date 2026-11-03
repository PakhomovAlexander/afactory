# ADR-0274: Orchestrate pinned Scenarios and admit Ant occurrences

**Status:** accepted (2026-11-02). Extends the publication boundary of
[ADR-0031](0031-deliver-verified-tasks-to-new-local-worktrees.md) only through a separately
confirmed Scenario action. Task delivery and Remote Checks are unchanged.

## Context

Tasks already own computational work, acceptance, sandboxing, plan approval and Provider budgets.
A workflow spanning a human review or an external merge needs durable coordination beyond a Task.
A scheduled binding also needs deterministic occurrence admission after downtime or writer loss.
Neither need requires an always-running model, a second implementation runtime, or implicit
publication authority from a source-writing Worker.

## Decision

1. A Scenario is a versioned, typed, ordered definition. Its five step kinds are Task, Scenario,
   Action, Decision and Wait. A Scenario Run pins its complete transitive definition bundle,
   machine policy, concrete inputs and resource limits. Authority names exact Task plans, action
   grants and decision owners; each child inherits a subset. Recursion and widened child authority or
   budgets are refused. Conditional skips retain their predicate identity and provide no output.
   Success requires the Scenario's own completion conditions and final output evidence. Typed
   receipt projections and scalar equality can drive conditions, including an evidenced
   no-worthwhile-optimization outcome without manufacturing a change.
2. Task steps execute existing Tasks through `af task run --confirm-plan`, naming exact Task
   revision and plan identities. The coordinator reads the Task Store for accepted results;
   process exit, PR checks and Scenario completion remain different facts. Generated plans still
   require their existing signed developer approval. Task delivery remains local and unchanged.
3. The coordinator has no model, arbitrary command field, shell, or workflow-defined environment.
   Machine policy, explicitly supplied as an owner-only file outside the checkout, permits exact
   Task plans, decision public keys, file roots and installed adapters. TOML is the authoring
   format; the versioned JSON wire documents are also accepted, selected explicitly by filename.
   A parse failure selects no fallback format or earlier version. The operator reviews and
   confirms a pin containing both definition and policy before creating a Run or registering an
   Ant. A changed adapter executable digest refuses dispatch. Existing pins retain their captured
   authority; cancel an existing Run to stop its future dispatch rather than editing its inputs.
4. A publication Action is a separate, narrow capability. `publish_pr` or `followup` requires an
   installed adapter authorized for that operation and exact destination, a Scenario grant within
   that machine scope, and a signed human decision for the exact Run, pin, step, object, revision,
   choice and deadline. A `required_condition` additionally binds observed evidence for the same
   revision. The adapter must enforce the current object revision as an execution precondition.
   This authorizes only the named operation through that installed adapter. It does not grant
   commit, push, merge, Task delivery or access to an arbitrary remote. No publication adapter,
   credential or active publication grant is installed by this change.
5. Adapters use a bounded, versioned stdin/stdout protocol with supervised deadlines, output caps,
   and process-tree termination. An intent, including exact input identity and stable idempotency
   key, is durably committed before dispatch. On every external Action, reconciliation precedes
   execute. Only an authoritative `absent` response permits execute with the same key; uncertainty
   remains waiting. Adapters must durably retain idempotent service receipts and enforce scope.
   Definitive failure may consume a finite new attempt; all prior intents remain recorded.
6. Decisions use detached minisign signatures. Signing keys remain outside af. Waits make one
   bounded observation per drive and return with a structured waiting reason. Changed revisions
   invalidate matching assumptions rather than reusing old green checks. Waiting consumes elapsed
   time; no model session remains open. Cancellation first records its reason, stops new dispatch
   and requests active Task cancellation. Completed or uncertain effects remain inspectable;
   cancellation never claims rollback. Pending child stops pin the exact Task run and are
   recovered by resume or repeated cancel under a separate delivery lock. A later manual run of
   the same Task is never cancelled by replaying that earlier request.
7. Built-in owned-file actions create an absent leaf file atomically with a versioned ownership
   wrapper. Cleanup checks the receipt's device, inode, owner and full bytes and preserves
   unexpected content, including a conflicting recovery claim. No recursive workspace deletion
   or unowned branch deletion is inferred from a path or label.
8. An Ant binds a Scenario pin and typed inputs to an explicit daily timezone, accountable owner,
   notification route, overlap/missed policies, per-run limits and rolling token limits. Add
   registers it disabled. Enable, pause and disable govern future launches; cancellation governs
   an existing Run. Paused/disabled intervals remain timed facts. Waiting Runs remain active.
9. `af ant tick` is a local, deterministic scheduler entrypoint. Immediate SQLite transactions
   admit unique occurrences by Ant revision and local date. Busy, missed, paused, disabled,
   nonexistent local times and spending refusals receive skipped outcomes. There is one active
   Run per Ant and no catch-up burst. Driver locks prevent concurrent dispatch; ticks recover
   admitted active Runs and poll their waits. Per-run token reservations include child work once.
   Rolling accounting holds full active reservations regardless of age and terminal consumption
   until a window after settlement, conservatively covering every dispatch in that Run.
10. UTC and fixed offsets have no transition horizon. IANA TZif transitions are captured in the
    Ant pin. A repeated local time uses its first instant; a daylight gap is skipped. V1 refuses
    beyond the explicit transition horizon when future POSIX daylight rules would be needed;
    a new reviewed binding with current tzdata is required. One tick covers at most 4096 calendar
    days, with explicit intermediate ticks for longer downtime.
11. The versioned, checksummed Scenario journal lives under `runs/` in the existing private,
    registered Store infrastructure. The ordinary machine Storage Budget counts it as Store
    remainder; no second budget or registry format is introduced. Durable coordination records
    are retained, with a 16 MiB document bound, and never silently collected. Current-release
    replay validates pins, port contracts, intent identities, signatures, progress and accounting;
    unknown shapes, missing journals and corruption fail closed (ADR-0145). Run admission allocates
    an opaque execution identity, retained in decisions and intents; equal labels in separate
    Stores cannot accidentally share external idempotency keys.

## Considered options

- A second model-driven orchestration engine: rejected because it would duplicate Task acceptance
  and require continuously retained model context while waiting.
- Arbitrary shell actions supplied by workflow definitions: rejected because input text could
  acquire ambient credentials and unbounded effects. Installed, pinned adapters are trusted
  machine extensions, with explicit operation and destination scopes.
- Treat a successful process or green PR as Scenario success: rejected because neither establishes
  the Scenario's declared completion conditions or accepted Task results.
- Retry an external Action after timeout without reconciliation: rejected because a completed
  service operation can lose its response. Durable idempotency and positive absence are required.
- Catch up every missed occurrence or release overlap while waiting: rejected because either can
  create concurrent work or spend a backlog without an accountable admission decision.
- Ambient local time or rereading updated tzdata on resume: rejected because a repeated tick must
  retain its original occurrence meaning. A bounded, pinned horizon is preferable to guessing.

## Consequences

Scenarios and Ants add runnable coordination without changing Task acceptance, delivery, gates or
sandbox boundaries. Operators install and audit adapters separately, sign exact decisions, and
schedule `af ant tick` externally if desired. They must explicitly replace a binding under a new
Ant ID when updating its pinned Scenario, policy or timezone. Afactory ships only inert examples
and isolated test fixtures; it creates no user instances, cron jobs, production schedules or
publication permissions. The operator guide and bounded adapter contract are in
[Scenarios and Ants](../scenarios.md).

## Amended 2026-11-02: goal roadmap planning milestone

The private Scenario Store also retains versioned optimization roadmaps. A partition represents
a complete bounded product-component, target-metric or optimization-goal loop, never a workflow
stage or day. Stable goal IDs are namespaced by an opaque roadmap identity; immutable parent-linked
revisions retain the full planning history. Untrusted estimates, proposed eligibility and source
leads are separate from runtime-owned reservations, actual charges and accepted native Task
receipts. Create and revise cannot import progress or acceptance claims, reset accumulated
accounting, remove/reuse goal IDs or change the captured definition of active/completed work.
Concurrent revisions compare exact content and parent inside an immediate transaction.

The journal shape becomes `af/scenario-store@2`, with no migration or fallback for v1. Roadmaps
share the existing ownership checks, registry, machine Storage Budget and 16 MiB journal bound;
history is retained as Store remainder. Native validation/create/show/list/revise are token-free
local operations. This milestone does not instantiate fresh Tasks, implement daily admission or
lab adapters, install a schedule, or activate a real Scenario/Ant. Task acceptance, Scenario
publication authority, Ant scheduling and existing Run accounting remain unchanged.

## Amended 2026-11-02: pinned fresh Tasks and complete-loop admission

Scenario definitions, bundles, pins, machine policies, Runs, lists and Ant documents now use v2;
roadmap proposals, revisions, roadmaps and lists also use v2. The private journal is
`af/scenario-store@3`. Previous shapes fail closed without migration. Existing exact-plan Task
steps remain available in the current shape. Signed decisions, owned files and the bounded adapter
protocol retain their original shape and authority boundaries.

A fresh Task step contains captured native Task-file bytes, exact committed catalog/pipeline/Worker
and Provider authority, repository/source identities, native limits and explicitly typed accepted
predecessor bindings. Its template digest must be authorized by both the Scenario and the machine
policy. It cannot carry arbitrary shell, mutable local packages, prefilled input references,
continuations, issue authority or a pipeline fallback. A native base selector is an exact commit.
`scenario template-pin` captures these identities without creating a Task or dispatching a model.
Generated plan authority is not implied. Every child inherits a subset of template grants.

Before dispatch, a durable intent reserves the declared Task allowance. Its opaque execution,
step and attempt identity determines a stable fresh Task ID. Ordinary native planning, exact-plan
confirmation and Task execution supply the plan, accepted result and accounting. Planning response
loss reconciles that same Task; changed bytes or authority cannot widen replay. Predecessor
receipts must match the exact completed accepted native result in the same Store. Native input
resolution retains artifact type, Snapshot and acceptance provenance, including candidate source
re-rooting. Missing, mistyped, foreign or unaccepted evidence fails closed. Cancellation also
requests stopping recorded fresh Task runs; uncertainty and completed effects remain retained.

A root Scenario can pin one roadmap identity and revision. Manual Run creation and Ant occurrence
admission share the private Store's immediate transaction to select one eligible goal, sorted by
priority then stable ID. Dependencies require completed evidence; conflicts are symmetric. A goal
owns a complete product-component, target-metric or optimization-goal loop. Its declared allowance
covers all Task attempts and is reserved before any model dispatch against per-run, UTC daily,
rolling-window and roadmap-total budgets together. Too-large or otherwise ineligible goals are
explicitly deferred, without narrowing scope or calling models. Ant skips retain a reason. Fresh
Tasks receive the complete captured goal as structured requirements. No stage-based claims exist.

Claims and accounting survive dates, future revisions and replacement schedules. Active/uncertain
claims hold their complete unused loop allowance regardless of age; known terminal usage counts
on admission and settlement UTC dates and through a rolling window after settlement. Known native
usage, including failed Tasks, remains separate from unknown usage. A finished Task with unknown
usage keeps the unused loop reserved. Native reconciliation can recover lost settlement responses;
no CLI can supply a charge, import a completion or duplicate a protected goal. Only pinned Scenario
completion with accepted native Task evidence and settled accounting completes the goal.

Performance model plans require Sol medium implementation, Terra high bugs, Sol high correctness
and Astra medium evaluation, all on `codex-personal`, without an advisory role. Verification has
no implementer. Default-light fallback is not authorized. Token-free command fixtures need no
Provider. Bounded research evidence and the asynchronous Performance Lab adapter remain pending;
typed Task outputs and bounded observation/reconciliation are explicit integration points, not an
installed adapter. The operator must complete one manual loop on Claw before authorizing its Ant
schedule there. Deployment, real benchmarks, VM mutations and Mac scheduling are not performed.
