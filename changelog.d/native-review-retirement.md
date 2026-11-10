- Finish native Review retirement: remove Campaign bootstrap and Registry readers, retain shared
  canonical Ledger integrity, and continue standalone Reviews with exact Base, history, prior
  claims and truthful Task accounting within existing bounds. See the
  [decision](../docs/adr/2026-11-10-retire-campaign-bootstrap-and-continue-native-review.md).
- Project authority now requires `af.toml` version 2 and Task defaults; Campaign routing is
  refused. Provider model configuration prints current Worker and binding templates. Changed
  closed contracts use RunEvent@3 and provider-models@4; retired formats fail closed.
