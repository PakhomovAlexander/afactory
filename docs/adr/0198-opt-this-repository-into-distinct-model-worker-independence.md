# ADR-0198: Opt this repository into distinct-model Worker independence

**Status:** proposed (2026-10-23); documents the owner-approved authority bootstrap `a5b6ff53`.

## Context

The owner explicitly approved Codex Personal only for this repository's implementation,
correctness/evaluation and bug review roles. The source authority already sets
`distinct_principals = false` and `distinct_models = true` in `.af/task-catalog.toml`.
This bootstrap authorizes the policy choice; it does not accept an implementation or its evidence.
Different models on one account remain the same account.

## Considered options

- Require distinct accounts: rejected for this repository because the owner chose one
  Codex Personal account with distinct models.
- Require distinct Provider families: rejected because all requested roles use Codex.
- Rely only on fresh sessions or distinct command Worker packages: rejected because neither
  establishes the required model diversity.
- Use the existing model-independence policy fields: chosen as an explicit repository opt-in.

## Decision

Retain the committed `IndependencePolicyV1` settings: `distinct_principals = false`,
`distinct_models = true`, `distinct_providers = false` and `command_workers_by_package = true`.
This uses the existing supported policy in
[`validate_independent_bindings`](../../crates/review-core/src/task/plan.rs); it changes neither
engine defaults nor other repositories. Distinct effective Worker packages remain required,
and command Worker package identity alone cannot satisfy distinct-model independence.

This run's explicit local bindings select Sol `gpt-6.1-sol` for implementation, Astra
`gpt-6-astra` for correctness and evaluation, and Terra `gpt-5.6-terra` for bug review, all on
`codex-personal`. Generic catalog Provider defaults remain unchanged. Operators must bind the
requested role packages and accounts through
[captured local bindings](../task-execution/local-bindings.md); Provider labels alone prove
neither account nor model identity. Admission must validate the effective bindings.

Fresh sessions, distinct Worker packages, minimum role-scoped inputs
([ADR-0028](0028-prioritize-wise-token-use-and-minimum-worker-context.md)), required checks and
independent review/evaluation, and exact-plan approval still apply. Existing deadlines,
budgets, sandbox boundaries and acceptance obligations remain binding.

## Consequences

The repository trades account diversity for model diversity on one account, retaining correlated
Provider and model-family failure risks. The policy permits account reuse; it does not claim
account independence or guarantee that every role differs from every other role. The compiler
enforces distinct models wherever independence is required, including verification against
source-writing Workers. Missing, incompatible or unadmitted role bindings must fail closed;
inconclusive or failed checks and reviews remain inconclusive or failed evidence.

## Planning fixtures

Tests that plan the repository's experiment, report and remote-check pipelines keep the
committed model Worker identities, contracts and Attempt bounds. Explicit local bindings use
isolated fake Providers for token-free catalog and identity probes; planning dispatches no
Worker and reserves zero Attempts. Regression cases require same-model and command-only
replacements to be refused under the copied repository policy, without changing that policy
or the generic Provider defaults.
