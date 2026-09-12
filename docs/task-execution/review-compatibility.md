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

## Required cutover

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

The trusted context adapter and legacy CLI still require this connection to be wired into common
execution. Canonical replay, inspection and broker currentness also need their adapter paths;
synthetic legacy Attempt lifecycle events cannot substitute for them.

Bounded Scatter must keep its parent DAG fixed while the common runtime owns every child
invocation and its accounting. Heavy Campaign continuation must retain one original Task
allowance and all prior spend across Round revisions. A wrapper around `Kernel::run`, disabling
its budget while leaving its Attempt ledger active, or creating a fresh Task for each Round
would not complete this migration.

Historical Campaign readers and resumes retain their original event types and identities.
Frozen fixtures remain byte-identical. New compatibility contracts and adapters are unreleased
until the entry-point, interruption, broker, Proposal, Scatter and heavy-continuation gates pass.
