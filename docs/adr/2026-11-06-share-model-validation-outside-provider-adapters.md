# ADR-2026-11-06-share-model-validation-outside-provider-adapters: Share model validation outside Provider adapters

**Status:** accepted (2026-11-06). Supersedes only the location decision in
[ADR-0148](0148-discover-provider-models-and-validate-every-binding-before-a-charge.md):
the validation authority and shared binding-diagnostic formatting move out of the Claude adapter.
All validation rules, model identities, effort vocabularies and Provider admission behavior stay.
[ADR-0147](0147-stop-the-claude-model-fallback-and-name-the-model-that-answered.md)'s native Claude
refusal and model-switch behavior and diagnostic words stay unchanged.

## Context

ADR-0148 placed validation for every Provider kind in `review_runner_claude::models`, beside
grammar G. As a result, the Codex adapter and neutral consumers in `af` depended on a
Claude-specific adapter to validate model/effort bindings and print binding refusals. Pi shares
these rules too. This dependency direction misstates ownership: catalog validation and binding
wording need no native Claude protocol or runtime interpretation.

## Considered options

- **Keep validation in Claude and re-export it from the common runner.** Rejected: ownership
  remains Provider-specific and reversing that dependency creates a crate cycle.
- **Copy rules and diagnostic formatting into each adapter.** Rejected: grammar, validation order
  and operator-facing words could diverge between planning, onboarding, the TUI and runtime.
- **Move all Claude diagnostics into the common runner.** Rejected: native safety refusals,
  model-switch accounting and unavailable-model interpretation belong to the Claude adapter.

## Decision

1. `review_runner::models` owns the existing `Kind` (Claude, Codex and Pi), catalog types,
   static/catalog validation, model resolution and refusal facts and labels. It contains the
   implementation, with no dependency on or re-export from a Provider adapter.
2. `review_runner::models::diagnostic` owns grammar G, safe identifier rendering, the bound-model
   description, binding-refusal composition and shared diagnostic display fragments. Claude's
   native hints use these fragments; common validation never calls a Claude formatter.
3. Codex, Claude and neutral consumers use the common API directly. The normal dependency from
   `review-runner-codex` to `review-runner-claude` is removed. Claude retains native refusal,
   model-switch and unavailable-model classification and composition, and extraction of its
   diagnostic's hint, including the common binding hint.
4. Move the model-validation and common formatting tests with their implementation. Keep native
   Claude outcome and hint-extraction tests in the adapter. Their functional classification stays.

## Consequences

The dependency boundary now follows the shared semantics. Runtime behavior, CLI spellings,
validation order, model identities, effort vocabularies, exact diagnostic wording, persisted type
strings, event names, schema IDs and JSON shapes are unchanged. This introduces no migration,
compatibility reader, alias, dependency outside the workspace or change to Provider admission.
The remaining decisions of ADR-0148 and ADR-0147 continue to bind.
