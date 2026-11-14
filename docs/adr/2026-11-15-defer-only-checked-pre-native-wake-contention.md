# ADR-2026-11-15-defer-only-checked-pre-native-wake-contention: Defer checked wake contention

**Status:** proposed (2026-11-15); candidate repairs for #935 and #936, live certification pending.

## Context

A valid recorder for a prefix-related Task could block Pi reconciliation because selection stripped
only the requested Task's prefix. Ordinary checked history or checkpoint contention also surfaced
as a terminal subprocess error, stopping an otherwise authorized finite Pi attachment.

## Considered options

- Ignore malformed selected recorders: rejected; removes trusted recorder validation.
- Retry subprocess errors by diagnostic text: rejected; can conceal corruption or uncertain admission.
- Retry every failure or extend polling limits: rejected; weakens finite bounds and native fences.
- Match complete recorder identities and type only pre-native contention: selected.

## Decision

Split recorder filenames at the final Task/run hyphen before comparing exact Task identities.
Use the existing run matcher for selected run-number validation. Preserve bounded private reads,
process/cancellation joins, checked source identities and atomic recorder revalidation.

Represent checked source-observation and checkpoint compare-and-swap contention with an internal,
nonserialized Store error variant. Invalidated history, corrupt checkpoints, changed identities,
closed/expired subscriptions and persistence errors remain refusals. An authenticated Pi poll may
return its existing empty response for this variant only, rechecking route authority and installed
host identity before returning. It stops before transport claims or native intent. Later work uses
the same attachment's existing finite timer, wall time and poll allowance. No persisted or public
shape changes, new admission claims, host calls or automatic native resends follow.

Debug observation seams reuse the CLI's numbered barriers. Deterministic public CLI regressions
pause after checked projection or before page commit, then commit a real heartbeat or competing
extractor checkpoint. Additional fixtures retain corruption/revocation refusals, selected-recorder
validation and no resend after native intent. Recording extension tests prove successful empty
responses retain polling without replenishing its bounds.

This follows the common Store and conservative uncertainty recommendation in accepted research
Document `sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`,
source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`.
There is no deviation from that recommendation or expansion of host authority.

## Consequences

Valid neighboring Tasks and ordinary pre-native concurrency no longer terminate eligible Pi polling.
Every other error remains terminal. Separate receipt stages, per-route progress, original-session
ordering, expiry, collection and sticky native uncertainty retain their existing contracts.
Independent whole-PR verification and final Pi/Codex live certification remain coordinator work;
Claude live testing remains deferred. No service, authentication or publication is performed here.
