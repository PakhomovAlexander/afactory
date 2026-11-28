# ADR-2026-11-28-admit-worker-session-continuation-by-reservation: Admit Worker session continuation by reservation

Status: accepted on reviewed merge (binding only when merged)

Date: 2026-11-28

## Context

Task Attempts currently start a fresh conversation. The private runtime directory and strict
reply schema give a Worker a bounded invocation. The Task host already binds Review Rounds
to their Subject, Snapshot, Base, scope and history. Native account and model probes establish
neither exact history restoration nor safe turn recovery. A Warm Set independently selects
Notes, Head Delta, Build Cache and Warm Workspace.

This decision adopts the recommendation of the captured design Document
`sha256:915af92acc8cea5c2d05b9cac97d719a8bd30ffe5a95af3a1304cc65d18b701b` from
`mac-worker-session-design-20261127-r5`, resolved result
`sha256:faa84ffb21156573a1aa3f1d9614264048999bbaec933354e9af5e8477e1faef`.
The design targets product commit `4208f9123590db0f939f7f34403d325ac7f8a902` and bound
Snapshot `sha256:4993f6711dff2d0dd9995116342866343ac5f6852d5783a68609c4d2130c2619`.
The prerequisite is authored against source `3fa653da8294f2cf994de5b7b8d325cf8ae855fb`.

## Considered options

- Use a global switch or limit continuation to first implementer repair. A global switch loses
  slot and shard ownership. An implementer-only path leaves Review and verification's typed
  admission boundaries without a common contract.
- Reattach by native thread ID or reuse the old home or workspace. Those methods can expose
  unadmitted turns, tools or filesystem state and mutate the predecessor.
- Restore the entire prior manifest or a native summary. Prior grants may have narrowed, and an
  opaque summary gives the host no proof of its bytes or dependencies.
- Fall back after a timeout. The original submission may still be live.

## Decision

Supersede [ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)
clause 6 only to permit the new Task-hosted, reservation-bound, role-neutral continuation
contract below. The old keys, events, artifact, readers, Cold Closeout and runner session layer
remain retired. All other clauses and historical captured authority remain in force.

### Planning and checkpoint identity

Trusted Worker packages and effective slots can declare `fresh` or `continue`. Omission resolves
to fresh unless a trusted slot supplies the declaration. Equal explicit declarations agree.
Reject conflicts in mode, fallback or scope at compilation with both origins. Capture separately
whether the first invocation may seed fresh, what to do without an eligible checkpoint, and
what to do without the required transport capability. Each ineligibility condition requires
refusal or an explicitly permitted and recorded fresh fallback. Unsupported explicit continuation
fails planning unless the plan permits fresh fallback. Every fresh invocation allocates a new
conversation, including on retry. Provider-controlled cache hits can occur in either mode.

The exact compiled plan captures resolved policy, origins, fallback conditions, authorized
lineage, resource limits, Worker binding and required transport capabilities. Candidate files,
model output, environment switches and discovered session directories confer no authority.
Before private transmission, recheck capability, isolation and admitted native principal under
[ADR-0090](0090-recheck-native-task-provider-identity-before-private-invocation.md).

Key the checkpoint by Task and captured plan, fully qualified call and node slot, logical role,
reviewer identity or verifier assignment, and Scatter owner, shard identity and slice scope
where applicable. Compare canonical package and prompt digests, Provider binding and principal,
model, effort, transport and protocol, tools, access, isolation, reply and port schemas, and
effective session policy digest. Record the producing Attempt and Round, source Snapshot,
Subject and Base where applicable, selected typed result, sealed outputs and predecessor.

Select only the exact admitted checkpoint on the reserved predecessor edge. Advancing source
requires an admitted transition, such as a selected sealed candidate or a committed successor
Review Round with the original Base. Default to same Task. An approved successor import names
both Tasks and plans, the exact checkpoint and compatible slot mapping. Reject unrelated
branches, authority drift and transfers across roles, sibling reviewers or shards. A retry uses
its reserved predecessor and admitted feedback IDs. Failed edits, partial replies and ambient
threads are ineligible.

### Reservation and current grants

Under [ADR-0066](0066-reserve-task-attempts-before-binding-exact-context.md), reserve the actual
Attempt and invocation with writer epoch, allowance, deadline and admitted feedback before
rendering the typed request and selecting its immutable predecessor outside the Store lock.
Under [ADR-0028](0028-prioritize-wise-token-use-and-minimum-worker-context.md), prove before
`ContextBound` and private dispatch that each item serves the current invocation's purpose.

Build an ordered Context Manifest for the restored prefix and current suffix. For each
instruction, message, prior output, tool call, argument, result, retrieval, attachment or
native summary visible to the model, record producer Task, Attempt, thread, turn and item,
origin artifact or Snapshot object and range, content digest and the current typed port or
precise grant authorizing those bytes. Validate retrieval scope, object, range and allowance.
Prior model content and summaries require a bounded grant for exact own-slot content and its
dependencies. Refuse opaque or unattributed items, private reasoning and uninspectable
expansion. Prior admission establishes provenance. Permission to run a read tool supplies no
grant to reinject its old results.

Use a versioned canonical encoding with ordered item kinds, roles, content and tool linkage,
unambiguous lengths and ordering, and a normalized prefix digest. Record per-item rendered
bytes and estimated tokens with estimator identity and totals for prefix, suffix and combined
context. Check per-item, retrieval, context, reservation and remaining Task bounds before
binding. Count the entire admitted context against the bounds even if the Provider later
reports cache hits. Preserve the admitted bytes and order, then append the current typed
inputs. Refuse restoration of a prefix containing a denied item. Truncation or summarization
of that item cannot establish exact continuation.

For Review, append the current Subject, exact Findings, selected evidence, scope and Round.
Refuse the full prefix if it includes a range missing from current grants or old Findings
without a specific current historical-evidence grant. Earlier conclusions retain historical
identity alone. A changed sandbox path conveys no prior file, command or tool authority.

Bind the manifest, exact native projection ID and digest, current request digest, checkpoint,
destination sandbox and reservation together. Establish equality of the installed prefix
before sending, including order, multiplicity and tool pairing. Replay recomputes the join
from immutable artifacts through `ContextBound` to dispatch receipt and rejects substitution.
An unstarted binding failure releases the reservation. A captured fresh fallback records the
refusal reason and uses a fresh-only binding. A bound context can never be rebound.

### Transport and output admission

The adapter declares bounded capabilities to create a fresh conversation or privately install
the exact projection, submit, observe or reconcile a turn, cancel and prove termination, and
export settled history. The kernel owns selection, authorization, settlement and promotion.
Apply the captured isolation and account checks before installing private material. Each
Attempt has a fresh sandbox from its authorized Snapshot, with newly derived cwd, mounts,
tools, access, environment and private runtime directories. Preserve read-only review and
required container isolation. Missing isolation refuses both continuation and fresh execution.

Run on a private disposable branch of the admitted history. Keep the predecessor immutable
and the Store, previous sandbox, account home and unrelated threads outside Worker inputs.
Require correlated invocation, settled thread and turn, terminal status, and the exact final
reply validated against captured strict schema and ports. Join this reply to the admitted
typed result and sealed candidate for source writing, the selected canonical Reviewer Result
and provenance for review, or the accepted typed result for verification and evaluation.
Promote the immutable checkpoint only with checked outcome admission under the current writer.
The same host contract serves compatible model Workers while preserving independent assignments
and existing Ledger, verdict, integration, delivery and publication authority.

### Dispatch fence, accounting and retention

Before submission can occur, durably record a single-use dispatch intent keyed by Task,
Attempt, invocation, epoch, reservation, manifest, projection, sandbox, principal and predecessor.
Serialize claims with the writer lease. Record native correlation and receipt, terminal
evidence and settlement as separate idempotent transitions. After restart, replay this fence
before any retry or fallback. Release provably unstarted reservations. A lost acknowledgement,
expired lease, timeout, missing PID or empty sandbox proves no absence of dispatch.

Reconcile uncertainty using a transport-supported stable invocation key or supervised sender
with durable proof of non-dispatch or terminal or quiescent execution. Correlate the exact
thread and turn, cancel outstanding work when needed and persist the evidence. Keep the fence
blocked with its reason when the host cannot establish non-dispatch or termination. Prevent a
second sender, and reject late outputs, promotions and settlements from a lost epoch. Terminal
evidence can clear the fence while the old output remains ineligible. Settlement alone leaves
an unresolved dispatch blocked.

Under [ADR-0209](0209-recover-a-lost-or-interrupted-attempt-without-charging-its-reservation.md),
the successor settles started writer-lost work as Abandoned and the current writer settles
host cancellation as Abandoned. Charge observed usage once and release the reservation.
Absent usage records zero charge with unknown usage. Abandonment uses the declared recovery
allowance. Ordinary Attempt limits, Task tokens, protected verification and deadlines still
apply. Definitive non-abandonment failure uses ordinary failure semantics. Deduplicate
cumulative observations by provenance per turn, preserve known and unknown totals on replay,
and neither recharge the predecessor nor infer zero usage from a cache hit.

Keep checkpoints private in the owning Task Store under the
[machine storage budget](0144-hold-afs-disk-use-to-a-machine-budget.md). Enforce export,
import, item count, bytes, age, retained generations and retrieval bounds before allocation.
Pin active predecessors and dispatch evidence. Use content-addressed integrity checks and
lease-aware cleanup. Never reuse abandoned sandboxes. Prune unreferenced payloads or expire
future selection through a recorded transition. Keep replay bytes until their owning Task
records retire together. Report eligibility loss and refuse corrupt or substituted bytes.

## Consequences

Merge this decision first. Introduce trusted planning, new version identities for changed
`af.pipeline/6`, `af.compiled-task/6`, `af/CompiledTask@4` and all changed persisted
execution, context and selection shapes. Use distinct types for checkpoint, history projection,
session context and dispatch receipts. Ship writers, strict readers, Store validation,
compiler revalidation, replay, schemas and parity fixtures together. Reject unknown fields
and prior types. Historical Stores stay with their pinned releases.

Prove shared admission with a deterministic transport, then implement the durable fence and
recovery, accounting and retention before enabling private sends. Qualify the pinned Codex App
Server executable and protocol on macOS arm64 with `codex-personal` and the actual Worker
launcher. Require isolated private history inspection and exact installation, new sandbox
root, enforced tool and write restrictions, strict replies, bounded events, account recheck,
and cancellation and process-group cleanup across launcher death or sleep. Qualify required
container isolation separately from trusted local execution. Enable only the exact supported
capability combinations, with evidence for each additional platform or transport.

Exercise fresh defaults, explicit fresh, seed, continuation and permitted fresh fallback through
`af task start` and restart with `af task run`, using an implementer, a reviewer across admitted
Rounds and an independent structured verifier or evaluator. Check exact typed admissions and
replayed receipts. Cover conflicting declarations, capability and identity drift, cross-slot
and cross-shard selection, unauthorized lineage, changed Findings and revoked ranges, cumulative
bounds, hidden native history, tool provenance, projection order and sandbox substitution.
Inject failures around intent, send, acknowledgement, terminal reply and checkpoint promotion.
Verify one reconciled dispatch, old-writer fencing, blocked uncertainty, usage and recovery
bounds, isolation refusal, and concurrent lookup and pruning. Inspection exposes effective
mode, provenance IDs, refusal reason and usage while keeping checkpoint contents private.

The prerequisite changes documentation. Its exact delivered candidate requires independent
whole-change verification and the workstream's required fast gate with release, functional,
e2e, uix, container and Markdown coverage. Draft feature PR #1266 remains incomplete pending
enabled transport, full role-neutral integration, Mac-local verification and hosted checks.
Live qualification requires paired fresh and continued Attempts with matched transition, role,
inputs, model, effort, isolation, tools, budgets and independently scored outcomes. Record
exact IDs, observed usage and unknowns, latency, retrieval and outcome. Include automatic cache
hits in fresh controls, disclose incomplete pairs and establish billed usage and price basis
before comparing spend. This decision provides no session execution or measured gains.
