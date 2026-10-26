# ADR-0226: Refuse startup projection reuse across authority callbacks

**Status:** rejected optimization (2026-10-26). Retains the freshness contracts of
[ADR-0064](0064-reuse-structural-validation-with-fresh-authority-checks.md),
[ADR-0096](0096-revalidate-task-execution-evidence-on-cached-replay.md) and
[ADR-0101](0101-reuse-review-structure-with-fresh-task-boundaries.md).

## Context

Issue #255 attribution identifies three expensive startup projections. The proposed narrow
change returns the projection from `checked_task_recording` alongside its plan, then uses it
in `TaskRuntime::with_store` instead of projecting again under the same reacquired Store lock.
It correctly keeps the projection after telemetry opening and the intentional unlock boundary.
However, `checked_task_current` projects before calling `authorized_plan`, which invokes
`TaskAuthority::validate_plan`, and before calling `authorization_current` when applicable.
The final existing projection therefore also validates artifact integrity after those calls.

Neither `TaskAuthority` nor `TaskDeveloper` guarantees immutable CAS files across a callback.
An immutable Rust reference and the shared Store mutex do not prevent filesystem changes or
writes through another SQLite connection. In particular, a scoped authority implementation
can validate through the real captured compiler and then return after a referenced revision
was removed or corrupted. The existing final projection refuses startup in both cases.
Reusing the projection taken before that callback accepts the corrupt-revision case.
The deterministic regression and exact commands are in the
[verification note](../performance-evidence/runtime-new-20261026/verification.md).

## Considered options

- **Return and reuse the checked projection under the reacquired mutex:** rejected. The mutex
  covers no CAS filesystem exclusion and the result predates authority callbacks.
- **Reuse the first projection across telemetry opening:** rejected. It also crosses the
  intentional unlock and misses intervening state/authority changes.
- **Declare callbacks pure, cache integrity, or add new synchronization and freshness machinery:**
  rejected for this attempt. These change the contracts or widen the requested experiment.
- **Keep the existing startup reads and retain the counterexample:** selected under the
  requirement to stop when the duplicate cannot safely be removed.

## Decision

Do not retain the proposed runtime or Store API change. Preserve the existing plan-only
recording API and its callers. Keep the fresh post-open authority check and subsequent
projection, all typed-envelope validation, writer/lease and exact plan checks, developer
approval and current authorization, Review Round and Integration fences, expiry behavior,
execution markers, replay and telemetry. A read returned from any helper grants no later
execution effect; durable dispatch retains its current fresh checks and SQLite transaction
fences. Domain handlers and external operations still run with the required locks released.

## Consequences

This attempt leaves the startup bottleneck unresolved. The new regression covers integrity
loss during the authority callback, refusal without an Attempt, same-lease recovery after
restoring exact bytes, one execution marker and successful reopened projection. It does not
prove a numerical performance gain or constitute a kernel Measurement. No cache, encoding,
telemetry payload, workload, oracle, gate, timing budget, cohort or release authority changes.
Any future optimization must preserve the post-callback integrity observation explicitly;
this record authorizes no broader redesign. Independent review and the enclosing unchanged
gate remain separate obligations.
