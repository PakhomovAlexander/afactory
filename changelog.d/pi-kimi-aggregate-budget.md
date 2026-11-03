- Pi Kimi now captures a conservative AF token-budget charge of total prompt plus completion
  tokens, without a cache discount. Absent cache counters remain unknown; reasoning and cache
  writes are never added twice. OpenAI accounting is unchanged. The adapter policy binds both
  backend conventions and refuses preceding policies before private input. See the
  [Kimi decision](../docs/adr/2026-11-03-admit-pi-kimi-with-explicit-unavailable-account-identity.md).
