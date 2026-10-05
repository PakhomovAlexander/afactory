- `af provider status` and the browser's Providers pane no longer list the Claude or Codex CLI's
  default context (`claude-ambient`, `codex-ambient`) when it has no login and a Provider of its
  kind is registered; it still shows on a machine with no Provider of that kind yet, and when it
  holds a login ([ADR-0140](docs/adr/0140-list-a-logged-out-default-context-only-until-its-kind-is-registered.md)).
