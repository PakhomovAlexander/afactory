# ADR-2026-11-16-recheck-owned-publication-after-identity-observations: Recheck owned publication after identity observations

**Status:** proposed (2026-11-16).

## Context

The installed workspace adapter checks signed authority before HTTP sends, but its owned push
previously relied on earlier admission. Local ownership, remote refs and parent observations
can block. An actor observed at invocation entry can also change before a later mutation.

## Considered options

- Rely on invocation entry: rejected because observations can outlive authority.
- Extend transport or publication deadlines: rejected because longer deadlines grant no authority.
- Reuse the exact signed guard at each mutation boundary: selected.

## Decision

The workspace publication guard observes the configured actor and numeric destination, then
checks the exact signed request and live expiry. Invoke it after ownership/ref/parent observations
immediately before a push. Invoke it again after HTTP connection establishment, before POST.
HTTP continues to disable reconnect and check remaining transport time after the guard.

This strengthens the timing in the owned-workspace composition decision's publication clause 3
and the HTTP final-guard decision. It supersedes no generic signed authorization, Wait, repair,
or driver clause. Performance proof validation remains independent. No persisted shape changes.
Dispatched-but-unsent POST uncertainty retains reconciliation-only recovery; it grants no retry.

## Consequences

Native local regressions use accepted command-backed Tasks and signed Decisions, real Git and a
bare remote. Transport fixtures change identity or advance their wall clock at the final blocking
identity observation, checking refused effects and retained native Task identities on restart.
They do not use production paid Providers or prove live GitHub, cancellation, physical reboot,
startup scoped publication or continuous driving. Those issue 834 Stage C obligations remain open.
