# ADR-0092: Capture common Review admission reservations

Date: 2026-09-13
Status: Proposed

## Context

The Task catalog cost in [ADR-0091](0091-capture-explicit-task-provider-admission-costs.md)
does not govern `af review run` or `af provider doctor`. Those common Review entrypoints still
captured a 4,096-token admission reservation. A successful native acknowledgement can charge
5,712 tokens (16,331 input minus 10,624 cached input plus five output); the same observation
without the cache discount charges 16,336. Increasing only the whole-Task allowance cannot
repair an individual reservation overrun.

## Decision

New common Review captures reserve 32,768 tokens and 45,000 milliseconds per distinct Provider
admission capability. This is an explicit new-capture default informed by observed native
context overhead, not a guarantee that future native usage will fit. Operators may supply both
`--provider-admission-tokens N` and `--provider-admission-wall-ms N`; each must be positive and
within the existing safe-integer range. `af review plan` exposes the selected per-capability
cost without running identity checks, admission or Workers.

Initial capture writes this choice into the existing `ReviewPlanSettings.provider_admission`
and compiles it through `OperatorAttemptCost`. The existing graph feasibility check protects
mandatory Review work and configured aggregate scopes before any paid Attempt. Selection may
perform its existing token-free local Provider identity read; this is not paid admission.
No Task token limit, Attempt limit, verifier protection, overrun rule or accounting path changes.
For uncapped policies the existing resource-envelope calculation includes the chosen cost;
explicit Campaign token caps remain admission limits. They do not impose a native-provider
consumption cap: actual usage can exceed a reservation, remains fully charged, and blocks
subsequent work when the captured budget is breached.

Resume uses the original captured cost, even for a 4,096-token plan. Omitting both options reads
that cost; supplying both must match its tokens and wall time exactly. A differing override
refuses before lease acquisition or common Task append. Later Rounds inherit the same policy.
Doctor and Review use the same capture and restoration functions, so Doctor admission is reused
by the exact Task instead of being paid again. Historical legacy Campaigns refuse these new
options. Task-file invocations retain the catalog policy in ADR-0091 and reject the Review flags.

There is no new persisted artifact generation: Review settings already required the exact
cost in every captured generation. Existing policy and plan bytes are neither rewritten nor
interpreted using today's default. Stateless `af/review-plan@1` gains a diagnostic
`provider_admission` cost object; it describes new capture, not an existing Campaign's policy.

## Alternatives and verification

Keeping an implicit 4,096-token new-capture default reproduces the measured failure. A free
probe, automatic retry, extra ledger, live usage prediction or allowance mutation would change
execution authority and is unnecessary. A higher default alone without captured replay checks
would still leave resumption ambiguous.

Synthetic native CLI tests retain the full cached/cold usage observations, prove 5,712 and
16,336 fit the new reservation, and prove 5,712 still breaches an explicitly captured 4,096
reservation despite ample Task headroom. They also retain an overrun above 32,768; check
aggregate refusal before any paid Attempt; and reopen Doctor with omitted, exact and different
bounds before running the real Reviewer. All tests use a local deterministic protocol fixture,
without credentials or model inference.
