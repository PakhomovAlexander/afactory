# Task settlement regressions in the telemetry laboratory

These are functional regressions, not performance measurements or acceptance. The implementation
uses the pinned Rust 1.88.0 toolchain and a private offline dependency copy. The full gate and
independent reviews run after sealing. The decision is
[ADR-0217](adr/0217-reapply-an-accounting-append-once-after-only-this-writers-heartbeat-renewals.md).

## Observed failures and what was reproduced

The supplied full-gate evidence reported an expired accounting-fixture lease and a Review Task
with outstanding Attempts in `telemetry_overhead_smoke`. It did not identify those outstanding
Attempts or prove that the earlier intermittent sequence conflict had the same cause.
An ordinary predecessor smoke passed locally; it did not reproduce that original incident.

The new Review regression cancels at the committed `ContextBound` record, between scheduler
preparation and worker dispatch. Before the dispatch repair it fails, exit 101, with public
`TaskAttemptAccounting` showing one `root.review.n2` reservation: started false, released false,
charged tokens 0, result absent, context present. That proves an admitted reservation was
stranded without executing a Worker. The runtime now releases that exact unstarted capability
through the fenced Store API. Its original scheduler failure remains recorded. The same test
then passes and assembles an incomplete, inconclusive Review conclusion; it never claims success
or releases started work.

A second regression supplies the exact projection an accounting append validated, then commits
a real renewal through a second connection before that append. Without the single revalidation
path, settlement fails its SQL sequence comparison and leaves the Attempt pending. With it,
exactly one settlement retains the observed seven-token charge, releases its reservation and
replays identically. An intervening usage change, successor takeover or expired writer still
refuses the append. No Worker, callback or paid operation is retried.

The accounting-fixture regression delays each setup boundary by 600 ms, spending over four
seconds on setup under its unchanged three-second lease. Renewal at the actual boundaries
keeps authority valid. It then observes seven tokens, waits for real expiry, takes a successor
and recovers with exactly those seven observed tokens under published main's ADR-0209.
The old writer cannot renew. This updates the lab-only fixture's former ten-token reservation
floor to the current recovery contract; no timeout, lease duration or golden is relaxed.

## Reproduce the focused checks

Use the repository's pinned complete toolchain and build in its own `target/`. In an AF sandbox,
copy the declared read-only Cargo seed into `target/cargo-home` before using it; the shared seed
is never writable input to Cargo.

```sh
cargo test --locked -p review-store --lib renewal_race
cargo test --locked -p review-pipeline --test it cancellation_between_context_and_dispatch
cargo nextest run --locked -p af --test it \
  -E 'test(telemetry_ledger::) or test(telemetry_bench::telemetry_overhead_smoke)'
cargo nextest run --locked -p review-pipeline --test it \
  -E 'test(cancellation_between_context_and_dispatch) or test(captured_command_review_uses_common_attempt_selection) or test(task_runtime::control::) or test(task_runtime::usage_recovery::)'
```

The focused nextest runs passed 21 ledger/smoke tests and 15 runtime, cancellation, lease and
recovery tests. Nextest's one-process-per-test isolation is the repository's existing runner;
no suite serialization was introduced. A concurrent libtest invocation exposed the existing
process-global model-binding test interference in the golden test; the unchanged golden passed
under nextest. Additional renewals initially interfered with deliberate corruption observers;
the raw observation helper retains its explicit "on the lease as it is" contract, and all three
corruption tests passed unchanged after that correction.

These tests establish the two deterministic failure paths above. They do not establish the
cause of every historical intermittent conflict, quantify a speedup, complete hosted telemetry
measurement or establish calibration readiness. The old cohort and its pinned collector remain
historical evidence; a replacement requires the committed repaired producer and the separate
[dispatcher procedure](performance-cohort-dispatch.md).
