# ADR-2026-11-03-admit-pi-kimi-with-explicit-unavailable-account-identity: Admit Pi Kimi with explicit unavailable account identity

Date: 2026-11-03. Status: proposed; accepted on merge.

## Context

The pinned official Pi SDK 1.4.0 supports Kimi Code OAuth and the `kimi-coding` / `k3`
ChatCompletions model. Its OAuth credential contains `type`, `access`, `refresh`, and
`expires`; it exposes no stable account principal. A profile path, Provider label or token
hash proves neither an account nor independence. Requiring an account for every Provider
would exclude this supported backend even where trusted policy explicitly permits shared
accounts.

## Decision

Supersede only the mandatory-known-principal clauses of
[ADR-0090](0090-recheck-native-task-provider-identity-before-private-invocation.md) and the
[Pi SDK host decision](2026-11-02-own-the-pi-sdk-host-and-confine-its-tools.md), for the
explicit pinned `kimi-coding` OAuth backend when trusted policy has
`distinct_principals = false`. All other identity, deadline, admission, sandbox, tool,
usage and publication requirements remain in force. OpenAI's stable `accountId` and token
routing-account comparison remain mandatory and unchanged.

Represent Kimi's account principal as `null`, with typed Pi `identity_mode = "unavailable"`.
The separately captured canonical auth-context digest is configuration authority, never an
account. Plans and admission receipts record this distinction; human plans and Provider status
say account identity is unavailable. Reject any unavailable principal under a trusted
`distinct_principals` policy, including differently named Providers, different profiles,
other backends and Command peers. The compiler also refuses standalone unavailable bindings
under that policy. Installed native Review has no trusted context-only policy and continues
to refuse unavailable principals; catalog Task Review can admit them under explicit policy.
No sentinel is an account principal.

The host rechecks backend, stored OAuth mode, canonical auth context, pinned SDK, model,
endpoint, thinking and tool policy before private invocation. Its guarded credential store
checks context and credential mode on every read and before and after refresh callbacks.
Kimi account substitution within the same context **cannot be detected**: Pi exposes no
stable principal. Operators accepting context-only authority accept that limitation. Token
rotation is permitted; secrets, labels and token hashes never synthesize an identity.

Use one backend adapter contract within the existing SDK host. Admit only official HTTPS
`https://api.kimi.com/coding/v1`, `openai-completions`, model `k3`, and the SDK's exact
`low`, `high`, `max` thinking levels. Never clamp an unsupported setting. Pi owns OAuth
transport/refresh. Ambient keys, helpers, custom OAuth hosts, model overrides, extensions,
hooks, shell tools and automatic inference retries remain excluded.

Observe actual native ChatCompletions chunks through `onProviderStreamEvent`: `usage` or
`choices[].usage`, with the SDK-documented cached-token alternatives. Missing counters remain
unknown; malformed counters refuse while preserving observed billing floors. Cumulative and
identical final observations replace one inference's counters. Completion tokens already
include reasoning; charge raw input minus cache reads plus output exactly once.

Changed contracts are `af/ExecutionPlan@3`, `af/TaskProviderAdmission@3`,
`af/TaskProviderContext@2`, `af/LegacyReviewTaskPolicy@6`,
`af/LegacyReviewInvocationPolicy@3`, `af/provider-doctor@4`,
`af/task-inspection@16`, `af/task-plan-inspection@2`, `af/pi-provider-setup@3`, `af.pi-host/3` and Provider registry
version 3. Writers, readers, closed schemas and parity fixtures change together; earlier
versions are refused with no migration or fallback (ADR-0145).

## Considered options

- Refuse Kimi entirely: retains unconditional known-principal authority but excludes the
  requested backend even under a policy that does not require account independence.
- Hash credentials, paths, profile aliases or labels: rejected because none identifies an
  account; refresh would also manufacture apparent account changes.
- Treat unavailable identities as distinct across backends or contexts: rejected because
  it would manufacture principal-independence evidence.
- Add arbitrary providers, endpoints or API keys: rejected; this decision admits only two
  pinned backend contracts, within the existing orchestration and supervision path.

## Consequences

The minimal `pi-light` family uses Kimi K3 high for implementation, Sol high for correctness
and Astra medium for evaluation, with mandatory local kernel and markdownlint checks and
unchanged light-family allowances. Verification has no implementer. Existing catalog families
and defaults stay as they are. Issue537 optimization remains for a subsequent actual Pi trial.
Hermetic tests and official-SDK mocked HTTP prove protocol behavior, never paid live success.
No live calls, credential inspection, remote gates, performance measurements or publication
are part of the implementation Worker.
