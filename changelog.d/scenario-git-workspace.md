- Add an optional accepted-candidate Git workspace capability that composes ordinary Tasks with
  existing signed Scenario publication and exact-head waits, including same-PR continuations and
  merge-gated owned cleanup. Dynamic publication requires explicit exclusive writer machine
  authority; the original server-locked adapter remains unchanged. Current Scenario/request/Store
  generations change and old state fails closed. See the
  [decision](../docs/adr/2026-11-06-compose-owned-git-workspaces-with-scenario-publication.md).
- Retain all present refs after an uncertain deletion, including same-SHA recreations. Add
  bounded exact-head completed-check outcomes and typed ordinary fresh-Task repair composition;
  pending checks and timeouts never trigger repair.
