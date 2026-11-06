- Add an optional accepted-candidate Git workspace capability that composes ordinary Tasks with
  existing signed Scenario publication and exact-head waits, including same-PR continuations and
  merge-gated owned cleanup. Dynamic publication requires explicit exclusive writer machine
  authority; the original server-locked adapter remains unchanged. Current Scenario/request/Store
  generations change and old state fails closed. See the
  [decision](../docs/adr/2026-11-06-compose-owned-git-workspaces-with-scenario-publication.md).
- Retain all present refs after an uncertain deletion, including same-SHA recreations. Add
  bounded exact-head completed-check outcomes and typed ordinary fresh-Task repair composition;
  pending checks and timeouts never trigger repair.
- Add native Scenario production-path integration coverage using a privately installed workspace
  and GitHub adapter, real Git and an inert transport. Verify accepted repair provenance, native
  signed continuation, one same-PR publication POST, ready/pending/stale outcomes and cleanup after
  a real merge. Retain the separate modeled composition and Python lifecycle crash regressions.
- Fix the installed adapter's continuation entry point to initialize GitHub transport for
  preparation that must read back its prior publication. Initial preparation remains offline.
