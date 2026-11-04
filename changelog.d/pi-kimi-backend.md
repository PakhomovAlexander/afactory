- Support pinned Pi SDK 1.4.0 Kimi Code OAuth with K3 low/high/max and observed native usage.
  Kimi account identity is explicitly unavailable; principal-independence policy refuses it.
  OpenAI account protections remain required. See the
  [identity decision](https://github.com/PakhomovAlexander/afactory/blob/main/docs/adr/2026-11-03-admit-pi-kimi-with-explicit-unavailable-account-identity.md).
- Add the optional `pi-light` implementation/review/verification family with Kimi high,
  Sol high correctness and Astra medium evaluation, using mandatory local checks.
  Changed plan, admission, inspection, Pi host and registry contracts refuse prior versions.
  Merged acceptance and report tests use the current plan, inspection and Provider registry contracts.
