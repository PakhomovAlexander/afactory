# Task wake foundations

The original issue #765 foundation introduced host-neutral contracts and a checked history
projector. The [journal and delivery continuation](task-wake-journal.md) now connects them to
opt-in original-session grants, registration before detach, finite driving, receiver persistence
and retention. See [operator setup](../task-wake.md) for current supported behavior and pending
live certification. The historical foundation requirements below describe the work this
continuation builds on; they are not a current claim that production commands are absent.

## Evidence and boundaries

The accepted native Document
`sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`
(source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`)
recommends common Store subscriptions/outbox plus independently authenticated host bridges. This
foundation follows that direction without activating any delivery. Its supplied SourceTree is
`sha256:c500e10f788b2e6955ad36739310f0952092c4f98a49cc142ea82a82fab038c0`,
Snapshot `sha256:aed61d0df9e0f0f3f3800eee43adfa951273c68eb89ed649a8082b9addf92a31`.
No separate probe-verification receipt was supplied here; coordinator host observations are context,
not independent compatibility certification. Claude live trials remain explicitly deferred.

`af-contracts::task::wake` describes bounded opaque metadata. It carries no prompt, credential,
transcript, raw diagnostic, native path, callback, environment or destination. Semantic state has
no `TaskStateV1.next_action`. Receipt stages are facts, not permissions, and `admission_unknown`
is explicitly neither admission success nor evidence of absence.

`af-contracts::task::lifecycle` shares pure phase precedence. Common runtime evidence reading is
shared with CLI state, which still owns command and diagnostic rendering. `af_runtime::wake::project`
revalidates checked Store history and CAS closure and returns ordered eligible observations with
an opaque checkpoint. Each page extracts at most 4096 new events with 1 MiB per row, 16 MiB buffered
history and the subscription's cumulative finite observation allowance. Later pages preserve
prior lifecycle boundaries. The projector writes nothing and invokes no model or host; the
journal continuation owns atomic persistence and documents checkpoint provenance and replay cost.

The pure projector accepts a supplied opaque Store incarnation; the journal continuation now
owns explicit durable allocation and ownership admission. Task opening/run identity must match checked history. Resumes, including RecordingResumed,
and a later execution after writer recovery establish later episodes. Maintenance leases, late
usage and cleanup do not. Final completion binds Finished and the checked result, including a final
unsuccessful result; abandoned cancellation remains resumable stopped execution. A synthesized event
stream terminal record supplies no authority.

Expiry observations bind lease epoch, deadline, causal event and observation time. An explicit fake
clock is sufficient for deterministic inspection, but this is provisional evidence: a later current
history may supersede the expired lease. A missing process never establishes an outcome. `project_with_run_facts` accepts original typed
run/cancellation records and validates their Task, run, process, writer and checked lease joins;
trusted integration must retain and revalidate those recorder files too. A repeated
scan can return an expiry at an already scanned sequence; durable insertion must deduplicate its
stable logical identity. Observation time is evidence, excluded from that identity. Once an envelope
has been materialized, retain its exact bytes/digest rather than regenerate it at a later clock.

## Original integration obligations

The foundation recorded the following obligations. The linked journal/delivery continuation
implements the user-facing workflow; its notes describe current limits and pending live acceptance:

- Allocate and admit an opaque durable Store incarnation. Fence restored copies and exclusive
  ownership; relocation preserves identity, recreation does not.
- Authenticate a scoped original-session route through independently installed host authority.
  Opaque references, registration and login never authorize inference or another Task.
- Persist registration, idempotency key/exact configuration digest, pre-dispatch baseline and launch
  intent before dispatch, including immediate completion. Same key/changed configuration conflicts.
  Reconcile uncertain launch from ordinary run records; a wake driver never launches work.
- Atomically insert every extracted envelope and advance its checked cursor in one Store transaction.
  Preserve every eligible boundary during downtime. Revalidate current source/CAS, lease epochs,
  cancellation/run facts and absence of superseding execution under the commit fence. Unknown or
  corrupt history and impossible cursors stop progress. Do not treat the returned cursor as a receipt.
- Serialize extraction with collection. Materialize through the collection boundary before CAS
  removal; otherwise block that collection step. Disclose collected source and count journals,
  outbox, receipts and deduplication tombstones in machine storage accounting.
- Enforce finite queue, bytes, attempts, age, request wall time, reconciliation and retention bounds.
  Persist retry schedules and sender fencing; restart cannot reset allowances. Retain receiver
  deduplication for the whole replay horizon and reject old expired keys after that horizon.
- Preserve delivery identity and exact payload digest across retries and subscriptions independently.
  Validate exact receipt bindings. Persist native admission intent before native invocation.
  Never blindly resend after ambiguous admission: authoritative reconciliation or proved native
  idempotency is required; otherwise retain `admission_unknown` without a replacement key or session.

No service installation or live host trial is performed by this candidate. The current operator
workflow is documented separately above; the experimental probe kit remains independent.

## Validation selection

The new contract unit/parity tests inherit the existing functional binary classifications. The
public-writer wake history fixtures live under the existing `task_runtime` integration module and
inherit its e2e classification. Store episode fixtures remain functional. Existing CLI state,
events, cancellation and resume coverage remains selected by the unchanged catalog. No gate,
fixture, budget or quarantine rule is weakened.
