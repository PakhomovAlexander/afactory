# ADR-2026-11-11-preserve-renewal-performance-workloads: Preserve renewal coverage beside waiter fixtures

**Status:** proposed (2026-11-11). Supersedes the benchmark event-mix consequences of
[Task observation](2026-11-10-reuse-validated-artifacts-for-task-observation.md), restoring
renewal coverage required by ADR-0163, ADR-0168, ADR-0170, ADR-0171 and ADR-0172.

## Context

Replacing lease renewals with pauses/resumes or handoffs removed existing workload coverage.
The Store computes renewal expiry as host time plus the production duration and requires a
strict advance. Two writes in one millisecond can therefore refuse a renewal on fast storage.
This is a valid production rule, not permission to change the benchmark's event semantics.

## Considered options

- Replace renewals with handoffs: rejected because different transitions remove renewal coverage.
- Increase leases, retry refused writes or synthesize timestamps: rejected because those alter
  production preconditions or conceal failures.
- Keep unversioned clock waits inside a timed renewal loop: rejected because the measured
  boundary changes and historical timing comparability would be misleading.
- Observe eligibility before one protected write: chosen, with bounded real-clock coordination
  and explicit timing treatment for each workload.

## Decision

Restore `state_projection_v2`, `state_projection_seed_v1` and `cli_state_fixture_v1`: open,
propose, admit, then exactly 9,997 production lease renewals. The full-output CLI oracle requires
10,000 ordered events and the exact renewal count. Their clock eligibility checks run in setup,
outside the measured CLI/projection regions. These are current main workload contracts, not
product compatibility readers.

Before each renewal, read the live durable lease expiry. If host time plus 60,000 ms cannot yet
advance it, sleep one millisecond and observe again, bounded by a one-second monotonic deadline.
Check expiry/fencing on each observation. Attempt the protected write once; a failed write
ends setup or the sample, classified by the Store's lease state. Backwards/stalled clocks fail
within the bound. No timestamp is fabricated and no lease or allowance is increased.

Persistence retains all 100 renewal transactions, ordered replay, logical-byte accounting and
its semantic oracle. A single setup renewal yields the original four-event prefix. Declare
`persistence_fs_v3` because its measured region includes the clock-eligibility reads/waits
before each renewal. The CAS round trip remains unchanged. The new cadence is not timing-comparable
with `persistence_fs_v1`; the replaced handoff workload `persistence_fs_v2` is refused.

Keep all six full-CLI waiter fixtures, CPU user-plus-system, wall, RSS and running/exit-124
oracles. Their setup uses pauses/resumes and live handoffs, ending in a normal fresh sixty-second
lease. Name the original-shaped two-node waiter writer `cli_wait_original_fixture_v1`, separate
from the renewal seed; the other five retain `cli_wait_fixture_v1`. Every write uses public APIs.

## Consequences

Renewal, projection and waiter workloads remain independently exercised without weakening
production renewal rules. Persistence clock coordination is visible measured work, not hidden
padding. Historical shared-host diagnostics remain historical, not current-head speed claims.
Runtime artifact validation, polling, cancellation, lease and startup handoff behavior is unchanged.
