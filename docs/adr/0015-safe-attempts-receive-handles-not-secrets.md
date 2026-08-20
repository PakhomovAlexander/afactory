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
