# ADR-2026-11-12-deliver-durable-task-wakes-to-originating-host-sessions: Deliver durable Task wakes to originating host sessions

**Status:** proposed (2026-11-12); not accepted. Milestone 1 of issue #765 introduces
experiments and this proposal. The subsequent [foundation milestone](../design/task-wake-foundations.md)
adds closed metadata contracts and lifecycle projection. The subsequent
[internal journal candidate](../design/task-wake-journal.md) adds paged extraction, internal
registration/outbox persistence and a conservative collection barrier. The subsequent
[delivery candidate](2026-11-13-drive-opt-in-local-task-wakes.md) implements opt-in public commands,
local host bridges and retention; final live certification remains pending.

## Context

Detached Tasks outlive the observing coordinator. A callback, terminal record or process exit
cannot establish durable delivery to its original interactive session. Native model admission is
an external effect: transport acceptance, inference admission, turn start and visible reporting
are different observations.

The accepted design Document is `sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`,
from source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`
(draft `sha256:3f2ee2156480106514517de4321bf9a5537c8dde7d8fba76766994368d58753b`,
sources `sha256:aeea4ca945d38a8452fb44787e57304d4687f56af70f7ba2ec5e304c668d5a87`).
Its recommendation is a Store subscription/outbox with separately authenticated original-session
bridges, bounded token-free driving and no blind retry of uncertain native admission. It supplies
no live host evidence or execution authority. This proposal retains that recommendation.

Rechecked against implementation source Snapshot
`sha256:eb48df8b53be3f87f6807dc87591d500d17d90ea13e2726e1467003d9da6cad7`:

- [Detached execution](../../crates/af/src/task_commands/runs.rs) still launches the ordinary run
  with empty stdin and its own process group. Its `af/task-detached@3` output has no subscription.
  The existing wait notifier is not an original-session bridge.
- [Event streaming](../../crates/af/src/task_commands/events.rs) still synthesizes the terminal
  record from one projection. The current printed schema is `af/task-event@2`, despite
  ADR-0250's historical `@1` examples. It is not a persisted completion event.
- [Public state](../../crates/af/src/task_commands/state.rs) derives plan and execution waiting;
  [state contracts](../../crates/af-contracts/src/task/state.rs) distinguish waiting, abandoned
  and finished. A cancelled outcome may be abandoned and resumable.
- [Store writes](../../crates/af-store/src/store.rs) retain FULL-synchronous transactional append;
  [collection](../../crates/af-store/src/store/task/collection.rs) retains a tombstone and can
  remove CAS closure. Neither supplies a notification collection handshake today.
- The Codex Worker remains ephemeral, the Claude Worker carries no session, and the Pi Worker
  owns an SDK session. Those adapters do not prove delivery into an interactive frontend.
  Newer Scenario/Ant scheduler and retention work does not change that distinction.

## Considered options

- Manual status/wait: retain as a truthful fallback; no recoverable automatic notification.
- Host-owned durable event followers: useful experiments, but each must solve registration races,
  consistent lifecycle projection, cursor atomicity and collection coordination separately.
- Explicit Scenario notify Action: appropriate for authored workflows; does not subscribe arbitrary
  detached Tasks or turn an Ant routing label into permission to send.
- SDK-owned coordinator: viable when deliberately chosen, but changes the host. Starting or resuming
  another application-owned thread does not demonstrate attachment to the original frontend.
- Common Store subscription/outbox plus host bridges: proposed because history extraction and
  retention correctness belong beside the Store while native host APIs belong outside the kernel.
- Shell callbacks, keystroke injection, log scraping and blind turn retries: rejected because they
  fail to establish authenticated original-session admission and can execute untrusted data.

## Proposed decision

### Observation identity and lifecycle

Allocate a durable opaque Store incarnation identity, separate from path, Task label and remote.
A recreated Store has a new identity. Relocation may preserve it; restored copies require exclusive,
fenced ownership before activation. Bind Task identity to that Store, validated Task ID, canonical
run ID and immutable opening identity. A subscription has its own opaque identity, exact scope,
expiry, caller registration key and configuration digest. Same key/different bytes is a conflict.
It never follows a new continuation Task implicitly.

A semantic transition identity binds Task identity, authoritative causal event ID and sequence,
notification kind, revision and lifecycle episode. Retain the checked source prefix separately.
`completed` derives from the recorded Finished change and checked result, never stream termination,
cleanup, late usage, repeated reads or process exit. `attention_required` describes human waiting;
`execution_stopped` describes abandoned resumable execution. Neither is completion. Reuse/extract
the current shared pure state derivation, with explicit time and cancellation/run facts; do not
invent an event-name classifier. Writer loss binds the persisted lease epoch and causal deadline;
observation time is evidence, not a new semantic identity. Revalidate absence of superseding work.
Resumes establish new episodes; subscriptions survive waiting until closure or bounded expiry.

Derive each delivery key from subscription and semantic transition, with a fixed payload digest.
Retries retain both; attempt numbers cannot create another logical wake.

### Registration, extraction and collection

Persist the authenticated subscription, pre-dispatch checked boundary and notification-enabled
launch intent before business dispatch. Failed registration refuses notification-enabled launch.
Catch-up for already waiting/finished Tasks is explicit and bounded. Registering after detach
returns cannot close the immediate-completion race. No transaction covers process creation:
reconcile launch uncertainty from ordinary run records, and never respawn work from the wake driver.

Place notification control state in the Task Store's SQLite transaction boundary, separate from
canonical Task events and acceptance/accounting authority. Validate each source prefix, derive
all eligible ordered lifecycle transitions, insert unique outbox rows and advance the cursor in
one transaction. Send only after commit. The cursor means extracted, not delivered. Scan every
boundary across downtime, including wait/resume/finish; unknown or corrupt history stops progress.
The synthesized terminal stream record remains output, never a new Task event.

Before collection removes CAS closure, reconcile subscriptions through its exact boundary under
the collection serialization and materialize bounded envelopes with checked provenance. Failure
or capacity exhaustion blocks that collection step. Materialized historical envelopes may survive
collection but do not replace full artifacts or prove current acceptance. Disclose missing source;
do not retain full source indefinitely merely for notification.

### Delivery, ambiguity and bounds

An installed bridge authenticates exact installation, ownership scope and original session. Persist
receiver inbox key/digest and scheduling claim before acknowledging `inbox_accepted`; identical
replays return the same receipt, altered bytes fail. Keep deduplication through the entire replay
horizon, then reject expired keys. Serialize native scheduling per session across subscriptions.
Busy work queues for an idle boundary without steering, interruption or concurrent inference.

Retain separate observations for outbox commit, durable inbox acceptance, native turn admission,
turn started and report observed in the original visible conversation. A return value or printed
packet cannot stand in for a stronger stage. Published report does not mean the human read it.
Persist native admission intent before calling the host. After lost acknowledgement or restart,
reconcile that same identity. Without demonstrated native idempotency or authoritative reconciliation,
record `admission_unknown` and stop automatic requests. Never create a new key/session to escape
uncertainty. Preserve causal order; an ambiguous send cannot silently be overtaken.

Require finite attempts, age, request wall time, queue capacity, bytes and reconciliation work.
Persist capped backoff and deterministic bounded jitter; restart does not reset limits. Retry
known pre-admission failure only within policy, and repeat transport only with durable receiver
deduplication or authoritative absence. Fenced sender claims prevent stale receipt commits but
cannot recall a transmitted call. Exhaustion remains visible and cannot erase uncertainty.
Count journals, receipts and deduplication tombstones within machine storage accounting. Refuse
new work when safe capacity is unavailable. Version future closed persisted contracts and ship
writers/readers/schemas/parity together; retain no earlier-release compatibility reader (ADR-0145).

### Authority, privacy and driver lifecycle

Register routes only through a trusted host grant binding installation, original session, owner,
Store/Task scope, permitted follow-up mode and expiry. Guessable IDs, environment values, repository
text, Worker output and login permission supply no grant. Use authenticated owner-controlled local
IPC with endpoint/peer verification and platform-specific provenance; no arbitrary URL or public
listener. Document trusted same-user assumptions. Refuse unsupported reconnect or identity change.

Payloads contain bounded opaque identities, causal references and structured state only, excluding
credentials, prompts, source, transcripts, findings bodies, raw diagnostics and native session paths.
Never execute callback strings, commands or environment values from packets. Inspection remains
an independently authorized bounded read. Host inference needs its existing exact-session follow-up
grant and accounting; subscription does not authorize login, increase limits or spend another Task
Attempt. A wake can inspect/report, never resume a Task, approve a plan, deliver, or publish.

Driving is deterministic and token-free. An operator explicitly starts/stops a bounded foreground
driver or separately configures a service/host hook, with startup/reconnect catch-up and visible
pending status when absent. Accepting a Task installs no service or schedule. Driver ownership,
revocation, finite retry defaults, platform authentication and collection integration need separate
review before production activation.

## Consequences

Durable observation requires bounded state, receiver deduplication, operator lifecycle ownership and
collection coordination. Some environments remain unsupported or receive-only; uncertain native
admission can require manual reconciliation. Exactly-once model execution is not promised.

Milestone 1 ships only the [experiment guide](../design/task-wake-host-experiments.md), closed
experimental evidence formats and opt-in probes. The Pi journal is a disposable experiment receiver,
not a production Store subscription or driver. Codex/Claude refuse when no original-session route
is established. Stand-ins prove probe behavior only. Live environment trials and independently
verified capability packets, followed by separately accepted implementation decisions, are required
before advertising automatic wakes.

The foundation milestone adds shared semantic projection and standalone versioned metadata
contracts. It does not implement the durable registration, outbox, authenticated bridges, driver or
collection barrier proposed above, and does not change this ADR's proposed status.
