# ADR-0068: Retain in-flight Task usage in the common budget

Status: accepted. Superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): usage
observations and settlements are encoded only as `TaskExecutionRecord@5`, the only execution
record, with decimal-text charges; no generation carries a numeric charge. GA has no Broker, so
its receipts are gone and the broker-to-common-Task receipt adapter this record still awaits is
never built.
Date: 2026-09-12

## Context

Broker receipts can expose paid usage before a Worker returns. Previously, common Task usage
observations required a settled Attempt. A crash between such a receipt and terminal settlement
could lose a known overrun. Canonical Review migration must retain this evidence through the
same Task accounting that owns dispatch, without introducing domain-specific spend accounting.

## Decision

`TaskExecutionRecord@1` usage observations accept trusted cumulative charge floors for any
started, unreleased Attempt. Duplicate or smaller observations cannot reduce or duplicate spend.
The budget moves observed tokens from the Attempt's outstanding reservation into committed
usage, retaining its remaining reserved credit. An overrun commits the excess and immediately
blocks new dispatch and further domain effects, including through an already started capability.

Settlement records retain their original bytes and exact idempotence rule. Their accounting
projection charges the greater of the terminal observation and the durable usage floor. An
abandoned Attempt still retains at least its original reservation. Writer-loss recovery uses
both recorded usage observations and the existing Attempt wall receipt; it cannot refund either.
Late usage remains recordable after approval revocation, settlement and source revision, through
the current writer lease. This is evidence retention, not renewed permission to execute.

The shared scoped token ledger performs partial observation accounting atomically across all
scopes and checks aggregate overflow before mutation. Legacy reservation/settlement callers that
do not publish partial observations retain their existing behavior and serialized contracts.

## Verification and limits

Budget regressions exercise two held reservations, duplicate and decreasing observations,
unstarted refusal, settlement without refund, overrun fencing, deadline expiry and later usage.
A Store regression reopens running work, observes an overrun after approval revocation, and
checks both lower terminal reports and writer-loss recovery with one paid Attempt. The actual
broker-to-common-Task receipt adapter remains part of the legacy Review cutover.
