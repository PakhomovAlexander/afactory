# Campaign Review on the Task runtime

`af review run` executions and the Provider doctor use the common Task runtime alongside
Task-file and embedded Review, for every Campaign. Missing common Task state refuses instead of
falling back, and a Campaign whose log holds events only the pre-Task executor wrote (af < 0.9)
is refused. This page maps the Campaign Review operations onto that runtime and records the CLI
boundaries. See [ADR-0084](../adr/0084-route-new-review-commands-through-the-common-task.md) and
[ADR-0113](../adr/0113-ga-reads-only-what-ga-writes.md).

## Adapter boundaries

The common runtime now reserves a real Attempt before context capture, binds the admitted
context, starts work, settles usage and publishes typed ports. It records scheduler diagnostics
and retries idempotent domain publication after a lost acknowledgement. A domain can read the
same serialized Store connection without holding its lock across a Worker invocation.

The Campaign Review adapter separates the following operations from Attempt accounting:

- Resolve only exact declared reviewer inputs, then bind the actual Attempt and captured Round,
  Subject, package and policy authority before rendering.
- Seal the Attempt's sandbox and capture the canonical flat Reviewer Result, its typed
  `TaskReviewAttemptProvenance` and a prepared or refused Proposal. Selection remains the common
  Task's responsibility.

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
([ADR-0068](../adr/0068-retain-inflight-task-usage-in-the-common-budget.md)). Provider
readiness and business calls retain separate captured policies and reservations in that same
Task budget ([ADR-0091](../adr/0091-capture-explicit-task-provider-admission-costs.md)).

## Installed cutover

The installed frontend now compiles the captured topology into an explicit public Task
contract, typed per-edge lanes and inherited Gate conditions. Flat JSON is adapted through
explicit codecs; existing envelopes keep their original identities and historical Snapshots.
`af/LegacyReviewRound@1` binds input capture to the exact current Campaign Round. Generation and
capture reopen without creating execution events. The compiler and operation host now exercise
common plan admission and execution, including the installed CLI cutover
([ADR-0069](../adr/0069-compile-captured-review-ports-with-explicit-artifact-codecs.md)).

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
ledger, retry loop, Worker transport or budget; the Task host composes that state. The
common runtime publishes domain invocation identity before reserving an Attempt and rendering
its context; lost acknowledgement is recoverable with the same durable invocation. New Task
dispatch and publication compare the captured Round under the same SQLite writer lock.
Ordinary dispatch stops on superseded or closed Rounds while late usage, settlement and release
remain recordable ([ADR-0070](../adr/0070-separate-review-domain-operations-and-fence-task-dispatch-by-round.md)).
Captured post-Round Integration and factual publication recovery use their separate, narrowly
fenced transitions; they do not reopen ordinary Round dispatch.

Canonical RunReport publication is also a domain operation. The Task supplies its exact
cumulative spend and accounting prefix, the conclusion is always `RunReport@6`, and automatic
Integration runs afterwards as the Task's own post-Round operation. Verdict ordering, cache
evidence and the one-conclusion guard are the domain's; publication adds neither an execution
ledger nor a new public verdict policy.

The trusted context adapter now uses actual common Attempts and canonical replay. Task-backed
inspection reads canonical reports and exact common accounting. Installed Review CLI and
Provider doctor use this path. Native invocations recheck their captured Provider identity
before private send; exact writer loss interrupts supervised work and retains observed usage.
See the [model and cancellation boundary](model-bindings.md).

The captured definition loader is now shared with the CLI, retaining package/policy validation,
Snapshot reachability and recorded light/heavy mode. Common compiled graphs support named
aggregate token scopes for retries and bounded child groups. Reservations and all usage count
against the same Run and matching scopes; graph replacement retains original scope charges.
Review's captured compiler assigns scopes by Campaign and numeric Round, preserving epochs
without resetting lifetime Task spend ([ADR-0071](../adr/0071-share-captured-review-authority-and-task-token-scopes.md)).

Bounded Scatter keeps its parent DAG fixed while the common runtime owns every registered child
invocation and its accounting. Heavy Campaign continuation retains the original Task allowance
and prior spend across Round revisions; each successor plan requires admission. The
[heavy Review walkthrough](heavy-review.md) records the shared history and acceptance boundary.

The captured Round can now compile directly from its recorded Campaign Manifest, authority
Snapshot and packages. The installed resource translator derives Worker timeouts, two-Attempt
retry capacity, four-way parallelism and one wall bound for the complete Gate check sequence.
An originally uncapped Campaign requires an explicit bounded fallback in the new Task policy;
its old manifest remains uncapped. Static Node caps retain aggregate retry charges, while
Scatter descendants share the captured FanOut cap without inheriting the static parent cap.
Round scopes include common Provider admission and use the numeric Round rather than its epoch.

`CampaignReviewPlanCompiler` now attaches exact Worker/Provider bindings, dependency wrappers,
invocation-policy digests, public evidence coverage and the common TaskAuthority admission
boundary. Original TOML and ReviewerPackage artifacts remain unchanged; typed dependency
wrappers retain their exact IDs and file closure. The Task policy is independent of the numeric
Round and records the bounded fallback and local execution choices. Native runners require
matching model/effort/backend bindings and the common Provider admission operation.

Plan validation rederives the graph and every dependency, binding, contract and policy from the
recorded artifacts without recreating missing CAS objects. Reviewer metadata, Gate outcomes
and Scatter evidence supplement the selected public outputs, including nodes not consumed by
the original Ledger. The operation host additionally retains mandatory Finding Set/Demand Set
companions, actual common Worker execution, canonical publication recovery and domain acceptance.
Canonical `RunReport@6` and the Task-backed Review inspection view retain exact cumulative
accounting. Owned Scatter execution, heavy-Round handoff and post-Round Integration now use
that same runtime.

Recorded-plan recompilation reads existing root wrappers and refuses their absence, different
producer, changed Round or changed head; it does not recreate missing CAS objects. Historical
Round reconstruction is separate from the active-epoch check used for dispatch. A Ledger's
`FindingSet@1` output carries the canonical Finding Set envelope, and every Ledger node also
publishes its `finding_set` and `demand_set` companions. A Scatter's slices answer `ReviewerResult@2`, like every
reviewer, so a Scatter must inherit Generation's exact Finding Set input.

## Common operation host

`CampaignReviewTaskHost` now executes the captured Command and packaged Model paths, shared pure
operations and Gates using common Attempts. The same slot-bound adapter performs Provider
admission and Review; account/model/effort/credential-mode substitutions are refused before
dispatch. A malformed response retains charge and supplies typed bounded retry feedback. Gate
materialization, container detection and each Check consume the original absolute deadline.

Publication recovery on either side of canonical commit preserves the selected Attempt and
original outputs. Typed Gate facts retain failed cache setup through settlement and Store
reopen, alongside existing successful cache receipts. Final Task acceptance reads the recorded
scheduler report and canonical Review verdict; blocking Findings and required Demands remain
unsatisfied even when execution completes. Conclusion publication compares both logs with the
current Task writer, and recoverable publication failures cannot be finalized. Recovery between
canonical conclusion and Task finish does no execution. See
[ADR-0077](../adr/0077-run-captured-review-operations-under-common-task-attempts.md).
