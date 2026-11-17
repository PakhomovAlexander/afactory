# ADR-2026-11-16-publish-implementation-under-native-startup-scope: Scoped native implementation PRs

**Status:** proposed (2026-11-16).

## Context

Issue 834 needs first and same-PR implementation publication within an owner-confirmed startup
scope without another human signature. Stage A admission and Stage B repairs do not provide
that capability. Strengthening generic signed guards cannot supply it.

## Considered options

- Widen generic `publish_pr`: rejected; exact signed decisions remain useful and narrow.
- Retain a signing key or synthesize a human signature: rejected; startup authority is explicit.
- Trust adapter JSON or output Snapshot affinity: rejected; native acceptance and source bindings
  establish candidate identity.
- Reuse performance authority: rejected; implementation publication has no measured-speed claim.
- Add continuous driving in the same change: deferred to a separate stage and lifecycle proof.

## Decision

Introduce `publish_implementation_pr_v1` under the exact owner-confirmed implementation startup
scope. This supersedes only ADR-0274 clause 4's later exact-decision requirement and the Stage A
publication exclusion for this new capability. Generic signed operations, performance authority,
Task delivery, Wait cadence and external human merge remain unchanged.

The effective recipe/template/operation pool, native source, repository, actor, destination,
branch namespace, budgets, repair count and expiry are admitted before paid execution. Command
Workers and remote gates remain refused. No runtime signing key is retained.

Native proof opens exact accepted implementation and independently accepted verification Tasks
from the Run's native template plans. Both cover the original complete goal and requirements;
verification consumes the exact candidate artifact through native predecessor bindings and pins
the original change baseline separately from live destination-base observations. Complete Review
Rounds, both reviewers, passing evaluator evidence and the native report remain required.

The proof binds the exact Run execution, pin, scope, intent, inputs and candidate. Immutable
acceptance validation can be cached; current authorization cannot. After blocking observations,
every external push, POST and PATCH rechecks actor/destination, source/base, active Run,
cancellation, current intent, bounds and trusted expiry. HTTP connects before the final guard,
disables implicit reconnection and recomputes timeout afterward; Git guards before spawning.

Production workspace and GitHub journals retain exact first/same-PR identity and metadata dispatch.
Unknown effects never become absence or success. Recovery reads the exact retained PR and never
repeats a possibly sent POST/PATCH. Same-head conflicting PRs refuse even with another base.
The PR body includes native Task reports. Only external humans merge.

Changed closed Scenario, Policy, Pin, Run, Store, Ant, adapter and workspace records take new
versions with strict readers, shipped schemas and parity fixtures. Earlier formats fail closed;
no historical Store or plan is rewritten.

## Consequences

The default generated impl recipe uses this explicit capability and requires admitted scope.
An explicit signed recipe remains available. Deterministic native Provider and remote fixtures
exercise the production proof and adapters without paid inference or publication. These are
isolated checks, not evidence of live credentials, VM reboot durability, continuous service,
default cadence or the combined whole-lifecycle proof. Those remain next-stage obligations.
