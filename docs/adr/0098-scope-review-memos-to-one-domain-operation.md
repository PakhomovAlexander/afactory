# ADR-0098: Scope Review memos to one domain operation

Date: 2026-09-14
Status: Accepted

## Context

PR2 memoizes canonical Round reductions and repair contexts. A digest identifies expected
bytes; it does not prove that the current Store still contains them. Reusing these results
across domain callbacks can conceal removed or corrupted original Reviewer Results.

## Decision

Serialize synchronous memo-using callbacks on each captured Review domain and clear both
bounded memos at every entry. Result assembly, context admission, output admission, result
validation and pure Review execution each establish a fresh operation. Nested reductions
share at most 64 Round and 64 repair entries only within that operation. Concurrent callbacks
cannot borrow another callback's memo. No memo entry authorizes the next operation, even
when its invocation and artifact IDs are unchanged.

Code checks and Worker execution run outside this operation lock. The lock never surrounds
model calls, subprocess checks or Task scheduling. Existing depth and artifact bounds remain
unchanged. This reduces repeated nested reconstruction within a callback while retaining fresh
integrity validation at every external domain boundary. It does not claim that cross-callback
work has disappeared or that digest identity is a durable integrity cache.

No wire generation, artifact identity, captured policy, acceptance rule or fixture changes.

## Verification

The existing real targeted-repair and two-Round CLI cases reuse one captured domain after
successful execution. Each original selected Reviewer Result is corrupted and removed between
callbacks. Warm and newly captured domains must both refuse; restoration must reproduce the
exact original outputs. The existing negative, missing, stale and rediscovered-finding cases
retain their original deadlines and semantic assertions. Full combined Gates remain required.
