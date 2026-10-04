- Add explicitly permissioned, private-host Provider authentication for browser-only setup and
  reauthentication, with Codex device approval, Claude code/callback support, bounded session
  state, recipient fencing and secret-free status. Generic setup login remains terminal-only;
  login never implicitly spends a model budget or resumes a Task
  ([ADR-0137](docs/adr/0137-permit-provider-logins-through-private-host-capabilities.md)).
- Include a runnable, consent-bound personal-chat host for validated one-time browser challenges,
  native Codex headless device login and Claude 2.1.289's exact unterminated prompt; reusable
  credentials remain native and ordinary af status stays challenge-free
  ([ADR-0139](docs/adr/0139-deliver-native-login-challenges-to-a-verified-private-requester.md)).
- Preserve native authentication failure categories alongside usage diagnostics and suppress
  credential-bearing failed output before ordinary capture, retaining exact parsed usage.
- Classify native login failures into closed response/proxy/TLS/rejection/transport states while
  keeping stderr private and bounded; retain the native lifetime guard and credential boundary.
