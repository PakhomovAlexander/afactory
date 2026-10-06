- A Task whose native Provider fails authentication at runtime is suspended instead of failed:
  the failed Attempt keeps its exact charge, the retry loop stops, and the Task waits as
  `needs_provider_auth` before any result is assembled. `af task recover` asks the Provider's
  token-free status first (a signed-out account returns the private-login handoff with no paid
  call), then verifies with one bounded probe charged to the Task's own ledger under the
  catalog's new `provider_recovery` allowance, and continues the same Task exactly once when every
  required context is verified. A login alone verifies nothing; a newer failure invalidates an
  older resume; each participation's own coordinator is notified (`--requester-ref`,
  `--coordinator-ref`, `af task recover --acknowledge`). Quota, model and network failures never
  ask for a login. A finished Task continues only through `af task continue`, an explicitly
  linked successor bounded by what its original limits left
  ([ADR-0141](docs/adr/0141-recover-runtime-provider-auth-on-the-original-task-ledger.md)).
