# ADR-2026-11-13-project-checked-task-wake-foundations: Project checked Task wake foundations

**Status:** proposed (2026-11-13); accepted on merge for this foundation only.

## Context

Issue #765 needs a common lifecycle interpretation before durable registration or host delivery.
The [wake proposal](2026-11-12-deliver-durable-task-wakes-to-originating-host-sessions.md)
remains proposed. Its accepted research Document is
`sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`,
from Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`.
The supplied implementation source is Snapshot
`sha256:aed61d0df9e0f0f3f3800eee43adfa951273c68eb89ed649a8082b9addf92a31`.
These identities bind research and source evidence; none grants execution or delivery authority.

## Considered options

- Match a few event names: rejected because waiting also follows checked execution evidence.
- Reuse printed state documents: rejected because they carry executable next-action text and
  describe only the latest state.
- Move command rendering into a shared dependency: rejected because inspection references and
  notification metadata must remain independent of commands, paths and presentation.
- Extract pure precedence and shared checked evidence reading: selected to preserve CLI behavior
  and replay every lifecycle boundary using the existing Store validation.

## Decision

Add closed version 1 subscription, observation, envelope and receipt contracts. Opaque digests bind
Store incarnation, immutable Task opening/run identity, subscription and registration configuration.
Attention, stopped execution and authoritative completion remain different kinds. Inbox acceptance,
native admission intent, native admission, uncertain admission, turn start and report observation
remain separate receipts. No receipt grants an effect or inference allowance.

The dependency-neutral `af-contracts::task::lifecycle` owns phase precedence. The common runtime's
`task_lifecycle` module reads checked evidence and shares existing failure/outcome rules with CLI
state. Command rendering and diagnostic formatting stay in the CLI. Store replay derives a lifecycle
episode from explicit ordinary resume, recording resume, gate retry and later execution markers.
Lease renewal/takeover alone and late accounting never establish an episode.

The read-only `af_runtime::wake` projector walks bounded checked prefixes. A completion cites its
actual Finished change and checked result. Lease expiry cites the persisted epoch and deadline,
with explicit observation time; it is provisional until current-history revalidation and fencing.
No synthetic observation enters canonical Task history. Delivery identity is independent of retry
attempts. A materialized envelope's exact bytes and digest must be retained for all retries.

## Consequences

These are foundations only. No Store incarnation is allocated, registration persisted, journal
created, driver installed, host invoked or Task dispatch changed. There is no automatic production
wake support for Pi, Codex or Claude. The [developer obligations](../design/task-wake-foundations.md)
are prerequisites for a later durable-registration and reconciliation milestone. Earlier-release
notification readers, migration or compatibility paths are absent.
