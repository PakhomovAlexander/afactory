# ADR-0274: Orchestrate pinned Scenarios and admit Ant occurrences

**Status:** accepted (2026-11-02). Extends the publication boundary of
[ADR-0031](0031-deliver-verified-tasks-to-new-local-worktrees.md) only through a separately
confirmed Scenario action. Task delivery and Remote Checks are unchanged.
Partially superseded for the new performance publication operation by
[ADR-2026-11-06-publish-verified-performance-under-standing-owner-authority](2026-11-06-publish-verified-performance-under-standing-owner-authority.md).

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
