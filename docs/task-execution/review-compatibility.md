# Review command compatibility

The legacy `af review run` entry point still uses its original execution owner. Task-file Review
and embedded Review already use the common Task runtime. The full increment requires the
legacy entry point to use that same runtime while preserving canonical Review evidence.

## Adapter boundaries

The common runtime now reserves a real Attempt before context capture, binds the admitted
context, starts work, settles usage and publishes typed ports. It records scheduler diagnostics
and retries idempotent domain publication after a lost acknowledgement. A domain can read the
same serialized Store connection without holding its lock across a Worker invocation.

The compatibility adapter extraction separates the following operations from legacy accounting:

- Resolve only exact declared reviewer inputs, then bind the actual Attempt and captured Round,
  Subject, package and policy authority before rendering.
- Invoke one Reviewer adapter, retaining its success, failure, panic and measured duration.
  This operation has no scheduler, retry loop, budget or Attempt ledger.
- Seal its sandbox and capture the canonical flat Reviewer Result, bounded provenance summary
  and prepared or refused Proposal. Selection remains the execution owner's responsibility.

`af/TaskReviewResultMetadata@1` carries the result contract, canonical result and provenance IDs,
and a closed Proposal disposition: absent, prepared candidate, or explicit refusal reason. Its
references preserve existing identities; metadata alone cannot admit a result, authorize an
operation or publish a Proposal. The actual Reviewer Result remains a typed result payload,
with metadata beside it rather than inside the frozen flat result.

The common Store also exposes a currentness check for an already-started Attempt. It checks the
exact prepared capability, active plan and approval, writer lease, reservation deadline and
absence of settlement. Domain effects must use this authority instead of a second ledger.

Trusted usage observations now commit cumulative spend during an Attempt as well as after
settlement. The remaining reservation stays held, an overrun blocks further effects, and lower
terminal reports or writer-loss recovery cannot refund known usage. Duplicate observations do
not double-charge. The original settlement bytes and exact replay comparison are preserved
([ADR-0068](../adr/0068-retain-inflight-task-usage-in-the-common-budget.md)). The actual broker
receipt adapter must feed these observations through the common Store.

## Required cutover

The installed frontend now compiles the captured topology into an explicit public Task
contract, typed per-edge lanes and inherited Gate conditions. Flat JSON is adapted through
explicit codecs; existing envelopes keep their original identities and historical Snapshots.
`LegacyReviewRound@1` binds input capture to the exact current Campaign Round. Generation and
capture reopen without creating execution events. The legacy CLI still awaits plan admission
and operation-host wiring ([ADR-0069](../adr/0069-compile-captured-review-ports-with-explicit-artifact-codecs.md)).

```text
captured Review configuration + exact Round inputs
                         |
                         v
              compiled Task execution plan
                         |
                         v
       common reservation / context / start / settlement
                         |
              one Review domain operation
                         |
                         v
             published typed Task output
                         |
                         v
        checked canonical Review selection + Proposal
                         |
                         v
             downstream gather / Ledger / verdict
```

The Store connection now binds the actual Task, plan, node, Attempt and published output to the
exact canonical result through `TaskReviewResultSelected@1`. It derives routing from the
Attempt's admitted `TaskReviewContext@1`, checks both logs in one transaction, and refuses
ordinary append of selection JSON. Receipt guards consume the selected result; Proposal guards
also require the selected side metadata's exact disposition. Reopening reuses one selection and
one Task charge ([ADR-0067](../adr/0067-project-common-task-selections-into-canonical-review.md)).

The canonical operations now live in a shared `ReviewDomainState` without a second Attempt
ledger, retry loop, Worker transport or budget. The legacy Kernel composes that state. The
common runtime publishes domain invocation identity before reserving an Attempt and rendering
its context; lost acknowledgement is recoverable with the same durable invocation. New Task
dispatch and publication compare the captured Round under the same SQLite writer lock.
Superseded/closed Rounds stop further effects while late usage, settlement and release remain
recordable ([ADR-0070](../adr/0070-separate-review-domain-operations-and-fence-task-dispatch-by-round.md)).

Canonical RunReport publication is also shared with the domain operations. Its execution owner
supplies the recorded spend, retaining the historical optional value for uncapped runs. The
legacy wrapper invokes automatic Integration after publication. Existing report versions,
verdict ordering, cache evidence and the one-conclusion guard retain their original behavior;
this extraction adds neither an execution ledger nor a new public verdict policy.

The trusted context adapter and legacy CLI still require this connection to be wired into common
execution. Canonical replay, inspection and broker currentness also need their adapter paths;
synthetic legacy Attempt lifecycle events cannot substitute for them.

The captured definition loader is now shared with the CLI, retaining package/policy validation,
Snapshot reachability and recorded light/heavy mode. Common compiled graphs support named
aggregate token scopes for retries and bounded child groups. Reservations and all usage count
against the same Run and matching scopes; graph replacement retains original scope charges.
Review's captured compiler must assign scopes by Campaign and numeric Round, preserving epochs
without resetting lifetime Task spend ([ADR-0071](../adr/0071-share-captured-review-authority-and-task-token-scopes.md)).

Bounded Scatter must keep its parent DAG fixed while the common runtime owns every child
invocation and its accounting. Heavy Campaign continuation must retain one original Task
allowance and all prior spend across Round revisions. A wrapper around `Kernel::run`, disabling
its budget while leaving its Attempt ledger active, or creating a fresh Task for each Round
would not complete this migration.

Historical Campaign readers and resumes retain their original event types and identities.
Frozen fixtures remain byte-identical. New compatibility contracts and adapters are unreleased
until the entry-point, interruption, broker, Proposal, Scatter and heavy-continuation gates pass.

The captured Round can now compile directly from its recorded Campaign Manifest, authority
Snapshot and packages. The installed resource translator derives Worker timeouts, two-Attempt
retry capacity, four-way parallelism and one wall bound for the complete Gate check sequence.
An originally uncapped Campaign requires an explicit bounded fallback in the new Task policy;
its old manifest remains uncapped. Static Node caps retain aggregate retry charges, while
Scatter descendants share the captured FanOut cap without inheriting the static parent cap.
Round scopes include common Provider admission and use the numeric Round rather than its epoch.

This captured compilation remains preparation data. Effective Worker invocation/Provider
bindings, exact policy and dependency capture, acceptance coverage and TaskAuthority admission
must be attached before it becomes an executable plan. The operation host, canonical replay,
common-owned Scatter and heavy-Round handoff remain part of the entry-point cutover above.

Recorded-plan recompilation reads existing root wrappers and refuses their absence, different
producer, changed Round or changed head; it does not recreate missing CAS objects. Historical
Round reconstruction is separate from the active-epoch check used for dispatch. Opaque v1 Ledger
ports use the captured finding identity policy to select either the canonical Finding Set
envelope or the frozen legacy flat encoding. Scatter result version follows its inherited
Finding Set input contract, matching the original executor.
