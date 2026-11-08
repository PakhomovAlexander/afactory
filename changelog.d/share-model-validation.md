- Move shared model/effort validation and binding diagnostics into `af-runner`, removing
  Codex's dependency on the Claude adapter while preserving validation and diagnostic wording.
  Claude's native refusal and model-switch diagnostics stay in its adapter
  ([decision](../docs/adr/2026-11-06-share-model-validation-outside-provider-adapters.md)).
