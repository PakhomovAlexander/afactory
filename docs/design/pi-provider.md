# Pi coding agent Provider

Status: implemented provider-neutral SDK host. The
[provider integration decision](../adr/2026-11-07-generalize-pi-provider-authority-with-explicit-profile-capabilities.md)
supersedes the earlier two-backend admission restriction.

AF supports **Node >=22.19** and the installed official
`@earendil-works/pi-coding-agent` **1.4.0** SDK, release
[`4a080252f06caf63a525d7873c5d94d3c23887e0`](https://github.com/earendil-works/pi/tree/4a080252f06caf63a525d7873c5d94d3c23887e0).
The stock CLI has neither AF's principal protocol nor confined file tools. AF embeds its
own ES-module host in `af-runner-pi`. Registration/status/catalog never install, update,
refresh a remote model catalog, log in, or run a model inference.

## Library boundary

`af` owns CLI presentation, machine bindings and native Task integration; `af-runtime` owns
Task execution and receives the Pi Worker adapter. `af-runner-pi` depends on `af-contracts`
for typed authority/usage evidence, `af-runner` for shared Worker interfaces, `af-process`
for bounded host supervision and `af-store` for retained evidence. Its embedded `host.mjs`
owns SDK interaction and credential access. Provider-neutral binding validation lives in
`af_runner::models`; Pi catalog, identity, framing and native accounting checks stay in
`af-runner-pi`. Neither the adapter nor its host depends on another Provider adapter, the CLI,
`af-scenario` or `af-project`. The [architecture map](../architecture.md#layout) describes
those application and library boundaries.

## Registration and effective catalog

Register an existing private profile and installed SDK explicitly:

```sh
af provider setup pi-personal --kind pi --backend openai-codex \
  --auth-dir /absolute/private/pi-profile \
  --sdk-entrypoint /installed/node_modules/@earendil-works/pi-coding-agent/dist/index.js \
  --json
af provider status --json
af provider models pi-personal --json
```

`--backend` is the exact SDK provider ID, including operator-registered IDs. Anthropic,
OpenAI, Google, Vertex, Bedrock and compatible custom endpoints use this same registration
and host path. AF does not maintain a backend or model allowlist. `ModelRuntime.getProviders`,
`getModels`, `getAuth` and explicit provider registration own provider/model/auth resolution.
Only chat models are Worker models; image/classifier catalogs are excluded. An empty chat
catalog refuses precisely. Status proves configured authentication, never paid usability.

The host reads only the selected backend's data from the explicit profile's `models.json`
and `models-store.json`. Pi's real composer applies data-only model declarations and
configuration; its offline refresh restores admitted cached overlays. No model network runs.
Configured or sufficiently recent cached `kimi-coding/k3.1-pro` is selected exactly when
present. The baked-in `k3` catalog is not a replacement for a requested ID. Model IDs include
SDK cloud/routed spellings such as `:0` and `~provider/model`. Account-qualified resource
ARNs are represented by deterministic `af-private-model/<digest>` selectors: their exact native
SDK IDs stay inside the host and are bound by the effective model digest. Bind the selector
printed by `af provider models`; it is a model identity, never an account principal. The
`af-private-model/` prefix is reserved for these generated selectors. Unsupported IDs or thinking
levels refuse before private prompt delivery. The catalog supplies thinking levels; session
construction must preserve the exact requested model and effort, without fallback or clamping.
SDK sampling parameters that override model, context/session, tools, transport or token allowance
refuse before private input. Pi applies these parameters after named request fields, so those
overrides would bypass the captured Worker authority. Ordinary sampling parameters remain supported.

Catalog pages retain the existing **64 models per ready record** and **64 KiB probe-output**
bounds. Each page carries a common full effective catalog identity. Rust checks identities,
contiguous offsets and duplicate IDs, within one shared probe deadline and a bounded 4,096-entry
pagination space. `af provider models ID --page N` selects a zero-based display page, also
bounded to 64 rows; JSON `catalog_page` carries `number`, `next` and `total`. Freshness is
`sdk_profile` (human: offline SDK/profile). `--resolve` checks the complete captured catalog,
including routed IDs with or without an effort suffix. Worker initialization requests its exact
selected model, independently of its
catalog position. Large SDK catalogs do not require a larger protocol record or a longer wall.

`--sdk-entrypoint` may be omitted when an installed launcher safely resolves to the supported
SDK. Managed `<agent>/bin/pi` discovery reads `install/current-version` as exactly `1.4.0`,
then resolves that release's `node_modules/@earendil-works/pi-coding-agent/dist/index.js`.
It never executes the launcher. Symlinked/malformed version files, unsupported versions and
release escapes refuse. Node and the SDK must be installed outside repository-owned paths:
an ancestor directory holds `.git` or `.af`, or the installation lies beneath the toplevel of
the Git repository containing the invocation directory. Outside a Git repository, the invocation
directory alone never makes an installation repository-owned. If the toplevel cannot be
determined, the invocation directory is treated as outside any Git repository; the ancestor
checks still apply. Each setup or probe operation discovers the toplevel once, using a
canonical, executable system Git at `/usr/bin/git` or `/bin/git` with no `.git` or `.af`
ancestor. Discovery never executes a Git from PATH, shell text or the installation.
Admitted provider modules also have every ancestor checked for `.git` and `.af`.
Homebrew's `.git` ancestor receives no exemption.

## Explicit credentials and installed provider capabilities

The selected profile must be outside repository-owned paths and owns `auth.json`; Rust never
reads its raw credentials. The host inspects
raw records using `AuthStorage.modify` with a no-write callback, never the helper-capable
`AuthStorage.read`. Stored OAuth and literal API-key records are admitted through the SDK's
provider-neutral resolver. Optional stored provider-scoped `env` remains confined to that
registration. Command-valued API keys or headers in data are refused before they can execute.

The optional private `af-context.json` selects environment/cloud credentials explicitly:

```json
{
  "schema": "af.pi-context/1",
  "environment": {
    "GOOGLE_APPLICATION_CREDENTIALS": "/private/selected-service-account.json",
    "GOOGLE_CLOUD_PROJECT": "selected-project",
    "GOOGLE_CLOUD_LOCATION": "selected-location"
  }
}
```

These are selected values, not names authorizing a copy of the developer's environment.
The Rust launcher clears its environment and creates a private HOME/XDG context. Only the
explicit selected provider environment is supplied to inference and exported inside that
host for official cloud SDK credential chains. HOME, PATH, XDG, Node/module injection and
loader environment overrides refuse. Credential-file paths must be absolute, canonical,
private owned regular files outside repositories. Explicit ADC/INI file references are captured
and checked recursively, within 32 files and a shared 4 MiB byte bound. Standard sectioned AWS
INI files are parsed as credential data; `credential_process` remains refused. Non-secret credential
selectors bind that closure separately from secret values. AWS profile selection also requires explicit
credential/config files. Command-bearing INI/ADC files refuse; installed provider code is the
separate executable capability boundary. No unrelated default developer profile is available.

Profile configuration files are bounded private owned regular files (mode 0600); context
and selected credential-file bytes are compared in memory before prepare, private prompt
acceptance, credential access/refresh and each inference. Literal API-key changes inside a
host refuse; OAuth token rotation remains supported under the guarded native identity/mode.
Secrets and credential-file contents never become emitted metadata or principal hashes.
Missing provider configuration or credentials produces a safe actionable AF refusal, without
claiming that the backend is globally unsupported.

An operator custom provider requiring executable SDK registration uses an explicitly installed,
reviewed provider module with a content pin, in the same private context:

```json
{
  "schema": "af.pi-context/1",
  "extension": {
    "entrypoint": "/installed/af-provider/bundle.mjs",
    "sha256": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  }
}
```

The entrypoint must be an external installed regular file with exactly those bytes; the pin
is rechecked before private inference. The host imports the verified bundle bytes, avoiding a
path reread between verification and execution. Bare/relative dependencies must be bundled.
Its exported `register` receives only a frozen facade
with `registerProvider(id, config)` and `registerNativeProvider(provider)`, restricted to the
selected backend. It receives no extension resource/tool registration API. Review and bundle
its executable dependencies as part of this trusted installed capability. No candidate module,
project hook, executable data helper or general Pi extension discovery runs. As with the
installed official SDK, this is trusted credential-bearing code, not OS isolation against a
compromised installation. It cannot widen the model-visible AF tool allowlist.

## Identity and captured authority

OpenAI Codex retains mandatory canonical `accountId` validation and agreement with the routing
account in the access token. Only that non-secret account identifier is hashed under
`af.pi.account/openai-codex\0`; neither tokens nor other credential fields become principals.
The guarded credential store rechecks that account on reads and both sides of refresh.

Other SDK authentication contexts expose no admitted stable native account identity. Their
principal is `null`, with typed `identity_mode = "unavailable"`; a profile, key, token, alias,
cloud project or Provider label never becomes a principal. Trusted `distinct_principals` policy
refuses every unavailable binding, across backends and profiles. Installed native Review still
requires known principals; catalog Task pipelines can explicitly permit context-only identity.
Account substitution across invocations within an unavailable context cannot be detected.

The captured authority separates backend, `auth_method` (`oauth`, `api_key`, or `context`),
SDK/Node paths and pin, canonical auth-context identity, exact model/effort and effective model
configuration identity. The latter binds the selected public profile/catalog configuration,
credential-context selectors, effective auth-supplied endpoint and installed module pin; key and
header values stay private. The adapter policy binds the embedded host and versioned native
accounting conventions. Rust compares this authority before sending a private prompt.

Current contracts are Provider registry **4**, `af/pi-provider-setup@4`,
`af/provider-status@6`, `af/provider-models@4`, `af/provider-doctor@5`,
`af/ExecutionPlan@4`, `af/TaskProviderAdmission@4`, `af/TaskProviderContext@3`,
`af/LegacyReviewTaskPolicy@7`, `af/LegacyReviewInvocationPolicy@4`,
`af/task-plan-inspection@6`, `af/task-inspection@20`, `af.pi-host/7`,
`af/pi-inference-evidence@2` and `af.pi-context/1`. Writers, readers, closed schemas and parity
fixtures change together; incompatible earlier versions fail closed without migration.

## Session, tools, supervision and accounting

Every Attempt creates an explicit SDK model/runtime, in-memory settings/session and an empty
resource loader. No project/global settings, skills, context files, themes, templates, packages,
MCP resources or warm session are loaded. Retries, compaction, cache warming and prompt expansion
are disabled. Provider HTTP retries are zero; transport remains SSE.

Read-only Workers get AF-owned `read`, `grep`, `find`, `ls`; writable Workers additionally get
`write`, `edit`. No shell, arbitrary tool or subagent exists, including for Workers allowed to
request checks. Only exact materialized source/base roots are readable; only a writable source
is writable. Symlink traversal and external/parent escapes refuse. Opens use `O_NOFOLLOW`,
file-type checks and bounded file/result/traversal sizes. Missing write parents are created
only beneath a checked writable grant. Native execution remains `trusted_unsafe`.

Rust sends init, compares ready identity/catalog, then sends prepare with exact model/effort,
configuration digest, trusted remaining allowance and reply schema. A matching prepared response
precedes private input. A fresh random 256-bit prompt nonce, first sent with that prompt, must be
echoed in accepted before usage or results can be admitted. Session success requires actual
`agent_settled`, successful final assistant text and return of `session.prompt`; `agent_end`,
EOF and exit zero are insufficient. Common `af-process` supervision owns cancellation,
the original Attempt wall, pipes and owned process group cleanup. Partial charges survive failure.

A terminal assistant `error` or `aborted` message, including one emitted after a completed
`toolUse` inference, records its `errorMessage` before closing the run. A rejected `session.prompt`
records its exception message when no earlier non-empty terminal diagnostic exists. Neither a later
final reply nor `agent_settled` can erase that failure. The `af.pi-host/7` inference-failure record carries
a safe diagnostic, a closed `failure_kind` category and an explicit accounting-completeness claim.
Rust validates the category and checks the claim against every inference's finalized native
observations and required counters. A resolved or rejected prompt with non-aborted settlement and
no usage, identity or lifecycle refusal can make that claim. Absent settlement, aborted runs and
unsafe accounting retain incomplete observed floors.

Complete native accounting survives a terminal SDK failure independently of Worker success.
The failed Attempt retains the exact native counters and aggregate AF charge, plus a separate
sanitized terminal protocol record in raw CAS evidence. The existing inference-evidence shape
stays unchanged: its `settled` flag still means successful host protocol settlement, and
`successful` stays false on failure. A complete failure is reported with its native diagnostic,
without an incomplete-billing refusal. The host classifies the first 4,096 characters of the original
terminal error before withholding or truncating its diagnostic, so URLs or credential material
cannot erase a quota, authentication or network classification. For a bounded JSON error, top-level
and nested `error` fields named `type` or `code` also contribute to classification when they are
scalar identifiers of at most 128 ASCII letters, digits or underscores, starting with a letter.
Recognized codes such as `rate_limit_error` and `model_not_found` classify even a neutral or absent
message. These fields never become diagnostic text. Rust uses the validated category,
independently of the filtered text. Failed Task Attempts retain the adapter's final category in an
Attempt-produced [`af/NativeFailureEvidence@1`](../../schemas/native-failure-v1.json) artifact,
bound to that Attempt's context. Task state and failure reports read this structured evidence
before diagnostic text, including when accounting is complete: a `rate_limit_error` whose safe
message is `model_unavailable` remains `provider_capacity`. Recognized native errors use AF's
existing classifications;
an unclassified SDK session or invocation failure is a Provider environment failure. Authentication
errors explicitly name authentication, so they cannot fall through to malformed Worker output.
Success still requires the same final assistant text and successful settlement.

Diagnostics are judged whole before truncation: non-string/empty errors get a fixed explanation;
inputs above 4,096 characters are withheld. The host remembers bounded initial/refreshed credential
values and selected private auth/environment/header values (including properties named `type`).
For `Authorization` and `Proxy-Authorization` headers (case-insensitive), it also remembers the
Bearer token or the Basic encoded credentials, decoded credentials, username and password, so a
credential echoed without its scheme is still withheld. Unsupported schemes, compound values or
unrepresentable Basic credentials cause all diagnostic text to be withheld.
It also remembers credential-file contents, JSON string values, INI values and opaque token lines
from the complete captured file-reference closure. For credential-source URLs it also remembers
every decoded query value whose key contains `token`, `key`, `secret`, `signature`, `password` or
`auth`, including repeated parameters. Unrepresentable query values fail closed like other
unrepresentable secrets. Oversized credential values/files, non-UTF-8
files or an exhausted secret-memory bound cause all diagnostic text to be withheld. It withholds
diagnostics containing private values, credential-like text or URLs. For a bounded JSON error it
selects only a string `error.message`, `message` or `error` and filters that diagnostic; other
structured payloads are withheld. It strips
terminal escapes and controls, collapses whitespace and emits at most 240 printable ASCII characters.
Rust applies the common native-output redactor again before retaining or displaying the diagnostic. Raw SDK error objects and stacks
never enter CAS evidence or operator output. Version 6 and earlier records fail closed without a fallback.

Native accounting is selected by **transport**, not backend. The pinned SDK's complete chat
transport catalog is covered:

| Wire contract | Observations normalized to AF counters |
|---|---|
| OpenAI Responses, Codex Responses, Azure Responses | `response.usage` input/output and observed cache/reasoning details |
| ChatCompletions, Mistral Conversations | Native usage aggregates and documented cached-token alternatives |
| Anthropic Messages | Uncached input plus native cache-read/cache-write components (omitted/null caches initially mean zero); output includes thinking |
| Google Generative AI, Vertex | Prompt total and total minus prompt, or observed candidate plus thought output |
| Bedrock Converse Stream | Native metadata usage input/output and optional cache components |
| Pi Messages | Native wire input/cache components and output, never initialized assistant counters |

OpenAI Codex preserves input minus observed cache read plus output, including its missing-cache
refusal. Other contexts use the conservative total-input-plus-output convention: no cache discount
and no duplicate charge for reasoning/cache writes already included. Additive components must follow
their native transport contract for complete accounting. Missing required components retain safe
partial floors and refuse success. Optional counters remain unknown unless the native contract
specifies an initial default. Malformed/conflicting counters prevent further inference.
No SDK-initialized assistant counters, estimates or service invoice claims substitute for native usage.
A custom transport lacking a native accounting contract refuses before private delivery.

Anthropic native `message_start` usage initializes omitted or explicit-null cache-read/cache-write
components to zero, matching the pinned SDK's transport contract. Subsequent deltas update only
non-null components, retaining previously observed cache values in both total input and the reported
cache breakdown. Mandatory uncached input and output still require native evidence; absent usage
never creates a successful zero-token inference. Negative, malformed, unsafe or decreasing counters
refuse while preserving safe observed charge floors, including when an additive total overflows.
The focused optional-cache regression runs real host sessions against both hermetic SDK signatures
and, when available, the installed official SDK's synthetic HTTP transport, without live requests.

Observations replace cumulative counters per inference; identical final echoes do not double count.
Every subsequent inference requires safe complete accounting for the preceding ones. `maxTokens`
is capped by trusted remaining allowance and the native output limit. One inference may overshoot
because prompt usage arrives afterward; its whole observed charge remains. Worker input cannot
increase the allowance. Existing bounds remain 256 inferences, 128 observations per inference,
4,096 tool executions, and the existing protocol/output/file/deadline limits. The inference-evidence record
retains counters, lifecycle, nonce and tool-name counts separately from aggregate charge, without
prompts, answers, tool arguments/results, credential values or account IDs.

## Regression evidence

`scripts/test-pi-host.py` always exercises the real embedded host with synthetic SDK signatures
and credentials. When an external official SDK is available it also constructs real SDK sessions
and streams against in-process synthetic HTTP/cloud transports, with no live model requests.
The catalog-driven invariant checks every official chat provider/model and all native transports;
non-chat models refuse. The matrix covers OAuth, Anthropic/OpenAI API keys, explicit Google/AWS
contexts, data-only compatible endpoints, installed registration, and configured/cached Kimi
`k3.1-pro`, including model rejection, drift, native accounting and actual settlement.

Rust runner tests cover protocol order/fragmentation, exact settings and auth drift, partial usage,
nonce acceptance, limits and cancellation/reaping. CLI/plan fixtures cover generic setup, status,
models, captured authority and common Task admission/typed-result execution using private HOME/XDG.
Schema parity/replay tests cover every new auth category and strict unavailable-principal refusal.
These synthetic tests establish no paid/live provider success. Official-SDK coverage is explicitly
reported as not run when unavailable; `AF_PI_TEST_HERMETIC=1` selects the hermetic path.
Historical measurement claims remain in [the validation record](pi-provider-validation.md);
this change adds no reconstructed workflow receipts.
The terminal-error regression drives `toolUse message_end` with complete usage, a late assistant
`error` carrying a distinctive diagnostic, and non-aborted `agent_settled`. Host and adapter tests
check retained diagnostics, complete charge, refused later replies and the successful final-stop
control, alongside invalid-usage and model-identity refusals. They make no live Provider calls.
The real-host adapter regression checks structured rate-limit and unavailable-model errors with
neutral messages, including the distinctive safe diagnostic in operator output and retained CAS
evidence. Both real-host paths also supply selected OAuth access and refresh values, profile
headers, AWS credential-file values, ADC private-key material and linked subject-token values as
terminal diagnostics, alongside a short secret echoed alone from a credential-source URL query.
They also echo a short configured Bearer token without its scheme. In-process callback tests cover
Basic credential components and fail-closed handling of unsupported authorization schemes.
The adapter checks that they stay out of operator errors and every retained
CAS artifact, and that withheld quota, authentication and network errors keep their categories
and complete charge. A common Task CLI regression proves that a completely accounted rate-limit
failure whose safe diagnostic says `model_unavailable` reports `provider_capacity`, retains the
safe diagnostic and charges the exact native usage after Store replay. Other category mappings
and diagnostic forms also run in process.

## Optional pi-light trial family

The repository pins only `pi-light/implementer`, `correctness`, `evaluator`, `review`,
`implementation` and `verification`. The implementer is Pi `kimi-coding` K3 **high**, using
only confined file tools. Both judges use Codex `gpt-6.1-terra`: correctness at **high**
effort and evaluation at **medium** effort. There is no extra reviewer. Verification is read-only
with no implementer. Kernel and markdownlint remain mandatory local checks; existing light-family
budgets, Attempt allowances and acceptance remain. No remote check is selected.

Register `pi-kimi`; the two Codex roles use the existing `codex-main` registration. Trusted
policy must allow context-only identity (`distinct_principals = false`); distinct-model policy
still applies. Existing families and default selections are not migrated. In a Task file set
`pipeline.name` to `pi-light/implementation`, with `pipeline.fallback = "refuse"` and the usual
implementation requirements, standard facts and resource limits. Execute explicitly:

```sh
af task start --file ../pi-trial.json --execute --json
```

For a subsequent verification Task use `kind = "verification"` and
`pipeline.name = "pi-light/verification"` on the implementation Task's recorded candidate.
The regression `pi_light_family_compiles_current_receipts_exact_roles_and_local_checks`
compiles both paths with private HOME, fake Provider executables and `env` present in fake
PATH. It proves current check receipt contracts and the exact role map without inference.
Issue537 optimization is intentionally left for the later actual Pi trial. These fixtures and
mocked HTTP streams do not establish live Kimi success.
