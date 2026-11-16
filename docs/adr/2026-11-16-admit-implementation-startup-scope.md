# ADR-2026-11-16-admit-implementation-startup-scope: Admit implementation startup scope

**Status:** proposed (2026-11-16).

## Context

An implementation Scenario must validate its entire captured recipe before spending on its
first Task. Checking each template only as it is reached can discover a broken repair branch
or missing machine credentials after earlier work has already spent its allowance.

## Decision

Introduce `af/implementation-startup-scope@1` as optional owner machine policy, confirmed by the
ordinary exact Scenario pin. It binds the complete bundle digest, exact template pool and model
bindings, repository and source, destination repository and actor, operations, branch namespace,
finite limits, repair count and expiry. No credential or signing key enters this record.

Native startup admission restores each declared template's durable native pin and captured
compiler, including conditional repair templates. It checks the complete Pipeline closure
and local replacements, refuses command Workers and remote gates, compares the explicit personal
model pool, and performs token-free Provider prerequisite probes. It also creates or reuses
deterministic, never-executed native startup plans against the captured source to check native
contracts, resource feasibility and local checks. Future predecessor content is validated again
by the ordinary resolver at actual Task planning. Each installed adapter must
answer a bounded startup preflight for the same scope identity. The workspace adapter verifies
its installed configuration, pinned tools and SSH configuration, local and remote source base,
and authenticated GitHub actor and repository. It makes no remote write.

The Run retains the scope through its immutable pin. Replay checks scope and recipe identity;
live startup and resume use trusted Host time for creation, elapsed budgets and expiry and recheck the full
native and adapter preflight. An inspection clock cannot supply authorization time. Dispatch is checked again after native
planning, its supervised timeout is bounded by remaining scope lifetime, and the watcher requests
cancellation on expiry. Cancellation
continues to stop future dispatch under the existing native cancellation contract.

This stage extends ADR-0274's startup admission only. It supersedes none of its generic signed
publication, Wait or repair clauses. Scope grants no new publication permission: `publish_pr`
and `followup` still require their exact signed decisions. Performance standing scope is unchanged.
Changed policy and containing persisted records take new versions under ADR-0145; no historical
Store or plan is migrated or rewritten.

## Considered options

- Reuse performance standing authority: rejected because implementation has no measured proof.
- Trust copied template assertions: rejected because they do not establish native captured authority.
- Validate only the first Task: rejected because later declared branches can require different
  machine prerequisites after paid work has started.
- Retain an owner's signing key: rejected because a startup capability must not synthesize signatures.

## Consequences

This is Stage A of issue 834. Typed unsuccessful-Task outcomes and bounded native repairs (B),
accepted-candidate scoped publication and continuous driving (C), and complete isolated lifecycle
proof (D) remain pending. Task wake APIs are not a Scenario service lifecycle. There is no merge
permission, daemon, new automatic publication path, or claimed performance improvement.
