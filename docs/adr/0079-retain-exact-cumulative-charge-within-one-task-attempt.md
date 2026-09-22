# ADR-0079: Retain exact cumulative charge within one Task Attempt

Date: 2026-09-12
Status: Accepted; superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): readers
for frozen execution `@1` and `@2`, usage `@1` and `@2`, and `af/review-report@1`, `@2` and `@3`,
and the historical `AttemptLedger` entry points with replacement semantics. Review inspection is
one document, `af/review-report@4`. GA has no Broker, so its Broker operations and
`BrokerOperationReceipt@2` are gone; the exact u128 cumulative charge stays.

## Context

One Worker Attempt can make several bounded Broker operations. Calls on one handle are
serial, but a paid call followed by a provider overrun of `u64::MAX` exceeds the previous
per-Attempt counter. Charging only the final operation, saturating the total or creating
artificial Worker Attempts would distort the Task's usage and execution history.

## Decision

Actual cumulative charge uses u128 in the existing BudgetLedger, TaskBudget and AttemptLedger.
Allowances, reservations, deadlines, verifier reserves and operation quotas retain their original
bounds. Exact observation and settlement preserve the largest recorded cumulative floor;
settlement does not add that total again. Arithmetic overflow refuses further admission.
Historical AttemptLedger entry points retain their original replacement semantics.

`TaskExecutionRecord@3` and `TaskTokenUsage@2` encode cumulative per-Attempt charge as canonical
u128 decimal text. Optional native token components remain individual u64 decimal counters.
The readers select the declared artifact version before parsing. Frozen execution @1/@2 and
usage @1 remain readable with their original bounds. The same wall-clock sidecar gains an
additive exact column: common Task recovery reads it before dispatch, malformed newer data
cannot fall back, and legacy narrowing rejects overflow. Existing historical bytes remain intact.

`BrokerOperationReceipt@2` retains one operation's full u64 observed charge as decimal text.
The exact Broker transport shares policy, credentials, containment, call ordering and revocation
with the legacy implementation. It exposes u128 cumulative usage to its trusted Task owner;
operations remain inside the Worker Attempt and consume its original reservation. The frozen
Broker receipt and legacy transport retain their historical numeric behavior. Captured operation
authority must fit the actual Task reservation, including a newly captured fallback for old
uncapped Campaigns.

The Review inspection format `af/review-report@3` widens current per-Attempt charges and usage,
while @1/@2 schemas remain available. Immutable `RunReport@6` accounting snapshots are unchanged.
Public schemas also describe the actual Task file, local catalog, compiled graph, inspection
and listing shapes; the Store's compiler comparison remains authoritative for compiled plans.

## Considered options

- A second Broker budget would split the Task's resource authority and duplicate reservations.
- Counting operations as Attempts would change Worker retry and concurrency semantics.
- Saturating or rejecting representable actual usage would lose paid evidence.
- Versioned exact accounting in the existing owner preserves both usage and historical contracts.

## Consequences

Current consumers need the new accounting and inspection versions. The common ledger remains
the only execution budget. Connecting the exact Broker transport to a durable Task-owned
binding and late-receipt ingestion remains part of the legacy Review cutover; the transport
and accounting checkpoint alone does not claim that connection is complete.
