# 0073 — Check Task retry eligibility before reservation

Status: accepted. Date: 2026-09-12.

## Context

An Attempt limit bounds the number of retries; it does not say which failures may be retried.
Legacy Review distinguishes malformed/contract/timeout failures from other transport failures.
Moving Review onto the common ledger must preserve that captured policy. A selected output
whose publication was interrupted must also recover without another paid Attempt.

## Decision

Before either common reservation API dispatches, the Store checks all durable Attempts of the
exact invocation. An unreleased pending Attempt blocks another reservation. A successful
settlement blocks another reservation and directs recovery to its selected output. Released,
never-started reservations permit a fresh reservation.

For settled failures, the trusted Task authority receives the exact invocation and a map of
prior Attempt identities to their persisted typed results. It checks retry eligibility before
any new reservation or execution event. The captured host delegates this domain policy without
adding a domain retry loop, Attempt ledger or budget. Existing generic Task policy remains the
default; the captured Review adapter must install its failure-class restriction.

Existing plan, approval, Round and writer-prefix checks still fence the Store transition. A
serialized failure, Worker response or remaining token allowance cannot authorize a retry.

## Alternatives

Using only `max_attempts` would widen Review retry authority. Checking only inside a Worker host
would allow direct Store callers to bypass it. Rerunning successfully settled work after a lost
publication acknowledgement would duplicate paid work.

## Verification

Store tests reopen the database before checking both reservation APIs. They prove pending and
policy-refused retries append no events, preserve prior charges, allow release of unstarted
work, and recover an existing selected output with one paid Attempt.
