- Add explicitly permissioned, private-host Provider authentication for browser-only setup and
  reauthentication, with Codex device approval, Claude code/callback support, bounded session
  state, recipient fencing and secret-free status. Generic setup login remains terminal-only;
  login never implicitly spends a model budget or resumes a Task
  ([ADR-0137](docs/adr/0137-permit-provider-logins-through-private-host-capabilities.md)).
- Preserve native authentication failure categories alongside usage diagnostics and suppress
  credential-bearing failed output before ordinary capture, retaining exact parsed usage.
