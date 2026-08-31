# Safe attempts receive handles, not reusable secrets

**Status:** accepted (2026-08-20)

Safe Execution Bindings never place reusable provider or service credential bytes inside an
executable sandbox. External operations use a non-secret broker handle bound to Campaign, node,
Attempt, and lease epoch; the trusted broker validates durable authority on every operation,
limits destination/method/resource and response shape, records usage, and rejects the handle after
fencing or cancellation. A runner that requires readable credentials is `trusted_unsafe` and
cannot satisfy safe review or automatic Integration policy.

## Consequences

- Killing a process is not the revocation boundary; the durable Attempt epoch is.
- Redaction remains defense in depth and is never treated as proof that candidate code could not
  read a credential.
- Provider adapters and authenticated source acquisition hold credentials outside reviewer
  sandboxes and expose only policy-bounded operations.
- Pipeline format v4 requires every reviewer to declare `credential_free`, `brokered`, or
  `trusted_unsafe`; formats v1–v3 retain their captured behavior and cannot claim safety
  retroactively.
- Project authority fixes symbolic operation routes plus request, response, call, and usage
  bounds. Machine-local policy supplies the connector and credential without capturing either.
- `ReviewerExecutionBound@1` records the exact Attempt lease. Each
  `BrokerOperationCompleted@1` receipt is durable before its response is returned, and durable
  replay independently checks the epoch, route, ordinal, calls, and usage against pinned policy.
- Credential-shaped response bytes fail closed before any response digest or body crosses the
  broker boundary. Raw authenticated wire bytes stay inside the trusted connector; the broker
  checks the decoded response for raw and common encoded credential representations, including
  mixed literal/percent bytes and nested percent escapes. Connector panics and usage outside the
  durable numeric domain become normalized charged failures.
- A v4 reviewer cannot be admitted without its durable Execution Binding. Attempt settlement
  covers every durable broker charge; crash, timeout, and supersession fences reserve the checked
  aggregate broker authority bound or higher observed usage so late completion can be recorded
  only as revoked. Terminal handle state is independently enforced during replay.
- Public revocation marks the handle before waiting for an in-flight call and withholds any
  overlapping response. A policy refusal leaves at most one durable acknowledgement before the
  handle becomes terminal.
- Live commitment is the maximum of dispatch reservation, Broker authority, and observed usage;
  terminal commitment is the maximum of settlement and observed usage. A late provider overrun
  therefore remains both receipted and charged after recovery or supersession.
- A budgeted v4 definition is rejected before dispatch when a reviewer's aggregate Broker
  authority exceeds the Attempt reservation.
- The current Codex and Claude CLI adapters are `trusted_unsafe`. A safe pipeline needs a
  broker-capable adapter; merely redacting a CLI's output does not upgrade it.
