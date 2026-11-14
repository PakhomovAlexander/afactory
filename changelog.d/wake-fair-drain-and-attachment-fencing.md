Fix wake drain starvation by persisting bounded extraction and native-route traversal positions
across invocations and restart. Fence Pi attachments and pending callbacks on disconnect, shutdown
and reconnect so old operations cannot send or change a replacement attachment. Deterministic CLI,
Store restart, schema parity and delayed-callback regressions preserve existing expiry, ordering,
backoff and uncertain-admission safeguards. See the proposed
[decision](../docs/adr/2026-11-13-fence-wake-attachments-and-persist-fair-drain-progress.md).
Live host certification remains pending; no service or host activation is added.
