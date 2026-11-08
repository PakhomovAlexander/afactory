- Clarify shared AF crate boundaries and internal naming. Performance tooling now separates
  offline result evaluation from durable release comparisons and uses snake_case Python modules.
  Public script paths, CLI commands and persisted contracts are unchanged. See the
  [library naming decision](../docs/adr/2026-11-06-name-shared-libraries-for-af-responsibilities.md).
- Integrate current-main GPT-6.1 catalogs, owner-default Workers, onboarding guidance, model
  provenance and Scenario performance coverage with the renamed libraries and Host boundaries.
  Refresh the default implementer instruction pins without changing their upstream authority.
