# ADR-2026-11-07-generalize-pi-provider-authority-with-explicit-profile-capabilities: Generalize Pi provider authority through explicit profile capabilities

Date: 2026-11-07. Status: proposed; accepted on merge.

## Context

The official pinned Pi 1.4.0 SDK supports provider-neutral model catalogs, stored OAuth,
stored API keys, provider-scoped environment/cloud contexts and explicit provider registration.
AF's initial host admitted only OpenAI Codex and Kimi K3. That finite backend/model map excluded
SDK-supported authentication and configured models, including a profile's Kimi `k3.1-pro`.
Replacing it with a longer AF provider allowlist would reproduce the same defect.

The SDK exposes `ModelRuntime.getProviders`, `getModels`, `getAuth`, offline model-store
restoration and explicit `registerProvider`/`registerNativeProvider`. These can be used without
Pi's project extension/resource loader or built-in file tools. An API key or cloud profile does
not establish an account principal. OpenAI Codex's native routing-account agreement remains
available and must remain mandatory.

## Considered options

- Add more backend-specific cases: rejected because every new SDK or operator provider would
  require another AF branch, and configured catalog overlays would still be excluded.
- Run the stock CLI or discover the whole operator/project profile: rejected because that admits
  unrelated resources, credentials, helpers and unconfined tools.
- Treat keys, tokens, profile paths, labels or cloud projects as account identities: rejected
  because none proves native principal independence.
- Accept SDK-initialized assistant usage or assume missing mandatory counters are zero: rejected
  because absent native accounting cannot authorize a successful result or further spend.
  Native optional-counter defaults, including Anthropic's initial zero cache components, remain
  part of the documented transport contract.
- Use the real SDK runtime with explicit data and installed executable capabilities: selected.

## Decision

Supersede only the finite provider/model/authentication admission clauses of the
[initial Pi host decision](2026-11-02-own-the-pi-sdk-host-and-confine-its-tools.md), and the
Kimi-only context-identity and model/endpoint restrictions of the
[Kimi decision](2026-11-03-admit-pi-kimi-with-explicit-unavailable-account-identity.md).
Retain their confinement, credential isolation, native currentness, accounting, challenge,
settlement, supervision, budget and publication requirements. The
[allowance decision](2026-11-03-bound-pi-native-inferences-by-remaining-allowance.md)
retains its bounds; its host/evidence versions advance with this change.

Use the pinned official SDK's effective chat catalog and provider-neutral auth resolution.
Read only the selected provider's data from an explicitly registered private profile's
`models.json` and `models-store.json`, using Pi's real data composer and offline cache
restoration. No network model refresh occurs. Select exact configured IDs and supported thinking
levels, including Kimi `k3.1-pro` when admitted; refuse fallback, clamping and non-chat models.
Represent account-qualified native resource ARNs through deterministic opaque
`af-private-model/<digest>` selectors. Their exact SDK IDs are captured by configuration digest
and rechecked inside the host; public metadata contains no native account component, and
these selectors never establish principal independence.
Catalog pagination preserves the existing per-record/output bounds and shares one probe wall;
pages must agree on the complete effective catalog identity. Worker initialization selects its
exact model independently of catalog position. CLI `--page` keeps display pages bounded to
64 rows too; `--resolve` uses the full captured catalog and disambiguates routed SDK IDs.

Admit stored OAuth, literal stored API keys and explicit provider-scoped credential environments.
An optional private versioned `af-context.json` selects environment values and external cloud
credential files. Clear the parent environment; create private HOME/XDG directories; expose only
that selected context to the SDK. Refuse loader/home/path overrides, repository-owned credential
files and executable data helpers, including command-bearing INI/ADC files. Capture explicit nested
credential-file selectors and recheck their bounded closure; secret bytes remain private. No whole developer
profile/environment or candidate configuration is available.

Executable provider registration is a separate explicitly admitted machine-local capability:
an external installed provider module with an exact SHA-256 content pin. Recheck that pin before
private inference. Supply only a frozen provider-registration facade scoped to the selected
provider, without tool/resource registration. The operator reviews and bundles its executable
dependencies as part of that trusted installation. This has the same installed-code trust
boundary as the pinned SDK; it grants no candidate extension discovery or model-visible tools,
and makes no claim of OS isolation against compromised installed code. Node/SDK installation
checks remain unchanged, including rejection of Homebrew ancestors containing `.git`.

Retain mandatory OpenAI Codex `accountId` and token-routing-account agreement. For other
contexts without an admitted stable native principal, record `principal_id: null` and typed
`identity_mode = "unavailable"`. Extend the existing context-only exception to those explicitly
selected SDK providers/authentication modes. Trusted `distinct_principals` always refuses an
unavailable binding across backends/profiles; native Review retains known-principal requirements.
Secrets and credential-derived hashes never become principals. Account substitution across
invocations within an unavailable context cannot be detected.

Capture exact effective model/settings and non-secret SDK/profile/catalog/module authority.
Recheck selected configuration, credential mode, file context and native identity before private
prompt delivery and inference, and on credential reads/refresh. Compare secret context bytes only
inside the host; never emit them or manufacture an identity from them. Retain safe actionable
AF-authored refusals for absent configuration/credentials, rather than globally unsupported
backend claims. Keep the confined AF file tools, exact roots, empty resource loader, cold session,
zero HTTP retries (including the AWS SDK's separate retry chain), original wall/cancellation and
common process supervisor. Reject sampling fields that replace model/context/session, tools,
transport or token bounds: the official `pi-ai/dist/api/openai-completions.js` and Responses
implementations apply sampling fields after named request fields, which would bypass that
authority. Ordinary model sampling and supported thinking maps remain available.

Normalize actual native usage by the SDK transport contract, independently of provider ID.
Cover every pinned SDK chat transport with a catalog-driven invariant. Preserve OpenAI Codex's
input-minus-cache-read-plus-output convention. Other contexts use conservative total input plus
output, with additive native cache/thought components observed where required. Optional counters
remain unknown except for documented native defaults: Anthropic `message_start` usage initializes
omitted/null cache-read/cache-write components to zero; later omitted/null delta fields preserve
prior values in total input and the reported cache breakdown. Mandatory input/output require native
evidence; an inference without native usage cannot settle. Malformed, negative, unsafe, decreasing
or missing required accounting refuses success and further inference; safe partial charge floors
survive refusal, overflow and cancellation. Initialized assistant counters are not native observations.
No backend-specific model substitution, fabricated mandatory zero, estimate or invoice claim is
introduced. An extension transport without a safe native accounting contract refuses before private
delivery; its provider ID is not globally unsupported.

Version current registry, host/evidence, printed reports and captured Task/Review authority
contracts together with their writers/readers and closed schemas. Old incompatible versions fail
closed under ADR-0145, with no migration or compatibility reader. Default/shipped models, role
maps, gates, retry allowances and publication authority are unchanged. The
[Pi design](../design/pi-provider.md) records the current formats and operator setup.

## Consequences

Adding an SDK-supported built-in or an operator data-only compatible provider/model needs no AF
backend branch. Installed provider registration is explicit and content-bound. Native account
independence is available only when genuine native identity exists, rather than inferred from
credentials. Catalog and authentication checks remain bounded and token-free.

Hermetic regressions exercise real host and CLI/Task paths. When available, the installed official
SDK also runs real sessions against synthetic HTTP/cloud transports and synthetic credentials,
without a paid or live model call. These are interoperability tests, never live service receipts.
No production login, installation/update, publication or reconstructed Task execution report is
a source deliverable.
