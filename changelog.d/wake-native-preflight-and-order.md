Repair Pi wakes claimed before an idle-to-busy transition by using the original session's followUp
queue while retaining attachment fencing. Reserve Codex preflight attempts and backoff durably
before endpoint contact, fence competing senders, and enforce native order over committed outbox
packets across subscriptions, including unclaimed packets. Add deterministic callback, restart,
contention, endpoint and reversed-claim regressions. Live Pi/Codex certification remains pending;
Claude automatic wake remains unavailable. See the proposed
[decision](../docs/adr/2026-11-13-reserve-native-wake-preflight-and-order-committed-packets.md).
