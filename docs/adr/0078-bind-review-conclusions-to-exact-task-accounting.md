# ADR-0078: Bind Review conclusions to exact Task accounting

Date: 2026-09-12
Status: Accepted; superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): readers
for historical `RunReport` versions and raw provenance.

## Context

The approved Task increment shares one lifetime resource allowance across preparation,
Provider admission, business work, retries and later usage observations. A provider counter
can use the full u64 range; the sum of several counters can exceed it. Frozen Review report
versions encode numeric counters only within canonical JSON's safe integer range. Dropping
usage, truncating it or refusing to record a completed Review would lose paid-work evidence.

## Decision

New Task-backed canonical conclusions use `RunReport@6`. Its cumulative committed charge is
canonical decimal u128 text, with an exact Task ID, revision, plan, scheduler report and
inclusive Task event sequence. The Store compares this receipt and total against the current
Task projection and checks both Task and Review prefixes under the append transaction.
Ordinary event append cannot create this accounting authority. Conclusion publication retains
current writer and approval checks while allowing an already exhausted execution to report.

An explicit execution variant distinguishes unbound, bound and cached Gate policy. It carries
the existing Gate binding, Check and Cache Snapshot facts. The same structural and durable
receipt checks apply even to incomplete conclusions. An unstarted Gate may have absent facts
only with an incomplete outcome; completed Gates require their evidence, and every reported
observation must match the durable facts. Captured execution policy remains explicit. Historical RunReport versions retain their bytes and readers.

Task reviewer provenance uses `TaskReviewAttemptProvenance@1`, bound to the actual common
Attempt and its admitted context, invocation, selected result and raw observation. Known usage
references `TaskTokenUsage@1` with exact decimal u64 components. Unknown usage remains absent
and charges the original reservation. This artifact records the transport observation; the
common ledger retains authoritative cumulative charges, including later observations. Provenance
cannot claim more paid usage than that Attempt has committed. Historical raw provenance must
match its frozen fields, Attempt, result, context and exact counters before a new selection.

Inspection separates the frozen cumulative charge at each report from current Task accounting.
Reports for the same Task are never summed as independent Round costs. Current charges come
from the common ledger, and Provider and business work count real started Attempts. Historical
Attempt classification uses its original plan. Late usage may increase current charge without
rewriting a prior conclusion or invoking another Worker. Final Task admission checks the same
ledger under the Task append transaction: a newly observed overrun rejects a stale satisfied
result, which must be reassembled as exhausted while retaining its original Review conclusion.

## Considered options

- Extend frozen numeric fields: rejected because existing canonical identities and consumers
  depend on their declared types and integer bounds.
- Refuse wide totals: rejected because valid paid work still needs a durable conclusion.
- Keep a separate Review budget: rejected because it would split resource authority and risk
  omitting Provider, retry or late charges.
- Use an explicit new report version and common accounting prefix: selected because it keeps
  historical readers stable and makes cumulative accounting independently checkable.

## Consequences

New inspection consumers must support decimal counters and distinguish historical snapshots
from current totals. The Store checks an additional Task prefix at publication. Gate contracts
and provenance remain strict, and the scheduler remains the only execution/accounting owner.
