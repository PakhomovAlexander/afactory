Restore the original 10,000-event/9,997-renewal CLI and projection fixtures alongside all six
waiter fixtures, using bounded clock-eligibility coordination before one protected renewal write.
Restore the persistence diagnostic's 100 renewals and four-event prefix; `persistence_fs_v3`
explicitly measures clock coordination and is not timing-comparable with v1. Separate the original
waiter writer from the renewal seed, regenerate the golden evidence and protected harness inventory,
and preserve production lease rules, budgets and exact output oracles. See the
[decision](../docs/adr/2026-11-11-preserve-renewal-performance-workloads.md).
