# Restricted Codex navigation transport

`af_runner_codex::navigation::run(Request, &dyn NavigationHost, &dyn NativeHooks)` is the
callable transport API. It starts the pinned native Codex app-server over stdio and exposes
typed paginated list, UTF-8 byte-range read and paginated text-search operations to a separate source-only broker.
The semantic tool set adds definition, references and implementation by source-relative Rust
path, zero-based line and zero-based UTF-16 column.
The wire request is the broker's shared `af_contracts::navigation::Request`; tool parameters
come from its version 3 schema. There is no line-to-byte conversion or implicit truncation.
The host connects `navigate` to `Broker::execute_until`, passing the same absolute deadline and
retaining the returned cursor/non-text fields. The [installed credential-free benchmark](agent-bench.md)
can use `run_fixture`, which binds IPv4 loopback and provides its own inert native auth home.

## Host contract

Supply an absolute native executable path and SHA-256 in `Pin`, plus exact restricted cloned
model catalog bytes and their SHA-256. Executables must be regular, nonsymlink files at most
512 MiB; hashing observes the same deadline. Pin the **native binary**, not the npm launcher (whose
interpreter and dispatched executable would otherwise be outside the pin). The supported
version is `codex-cli 0.167.2`, experimental app-server stdio protocol. The catalog has exactly
one model, `gpt-6.1-sol` for subjects or `gpt-6.1-terra` for graders, with `tool_mode`, `multi_agent_version` and `apply_patch_tool_type`
explicitly null and `experimental_supported_tools` empty. The fixture catalog is a clone of
locally installed metadata with neutral instructions; its bytes are synthetic conformance
inputs, not evidence of current account access.

`Request` binds the source, Task/Attempt/invocation, prompt, tools, catalog, executable, opaque
native account identity, protocol and configuration. Use `prompt_identity(question)`,
`ToolSet::identity()` and `config_identity()` for the respective
bindings. The trusted host supplies the account binding. The official gateway verifies its SHA-256
against the native ChatGPT account header. It never reads a token to identify the account. Supply an absolute monotonic deadline, a positive maximum
number of sends (at most 256) and a cumulative inclusive-token stop threshold. The threshold
stops before the next send; an overshoot also fails the invocation. It is not an intra-call
output cap.

`native_auth_home` is an operator-prepared native Codex auth directory. At admission it may
contain only a regular `auth.json`; ambient configuration, skills and instruction files are
refused. The transport does not open that file. Native Codex reads its own authentication.
Do not point this API at the developer's ordinary Codex home. Codex may write native runtime
files there, so the host must own its lifecycle and prepare a clean native auth directory
for each invocation. The transport uses a fresh empty working directory and HOME, explicit
neutral base/developer instructions, ephemeral threads, empty environments, read-only intent,
never approvals and no fallback. The typed subject profile uses medium effort and the grader
profile uses high effort. Project documents, skills instructions and
bundles, MCP configuration, web search and every feature key from the accepted probe are
disabled. This transport restriction does not prove source containment: only the broker
should have a source mount, and no checkout or oracle is supplied to app-server.

Implement `NavigationHost::navigate(Navigation, deadline)` with the separate broker. Its only
operations are `ListPaths { offset }`, `ReadRange { path, offset, length }` and
`Search { query, offset }`, with the exact version 3 broker bounds. Offsets are path indices,
UTF-8 byte indices and hit indices respectively; they are never line indices. The transport
retains its additional 64 KiB callback-response bound and refuses larger broker responses. Unknown fields, tools and namespaces fail closed. The broker must independently reject
traversal, escaping links, special files, execution and network operations. Callbacks must
honor the supplied deadline; the transport cannot preempt arbitrary Rust callback code.

Implement `NativeHooks::before_send(intent, deadline)` by checking the ordinary native
reservation, remaining scope and deadline, and durably recording the globally unique intent
before returning `DurableAck { intent_id }`. Implement `collect(record)` by durably retaining
that exact raw record before acknowledging the same ID. These hooks are the integration seam
for the existing ledger, not a second reservation/settlement system. Hook errors and incorrect
acknowledgements stop the invocation. Hook error prose is not retained, because it could
contain credentials.

## Gateway and raw records

The gateway binds a random IPv4 loopback port and requires an independent 256-bit random
`X-AF-Gateway` capability. It strips that capability and forwards only native `Authorization`
and `ChatGPT-Account-ID` values, plus the fixed content type. Official requests require the
account header to match the operator’s binding. The dummy endpoint has a separate inert binding. It never reads tokens
from native storage, refreshes them, or records authentication headers. The temporary gateway capability is supplied through an environment-backed native header,
not a literal command argument. Do not log the private process environment.

Production upstream is the constant `https://chatgpt.com/backend-api/codex`. Only GET `/models`
and POST `/responses` are forwarded; POST `/v1/responses` is deliberately translated to
`/responses`. Arbitrary URLs, queries, compaction routes, CONNECT, upgrades, WebSockets,
ambiguous lengths, duplicate headers, redirects and ambient HTTP proxies are refused.
Compaction is unsupported and fails closed; it never bypasses native admission. Any subsequent
ordinary inference or explicit retry uses a new guarded intent. There are no transparent
HTTP or stream retries. TLS verification and the bounded upstream exchange use `/usr/bin/curl`
with its ambient configuration disabled, supervised by `af-process`; credentials travel only
in its stdin. Curl is an additional trusted platform prerequisite (tested here with 8.7.1).
On macOS curl runs under the same pinned deny-fork launcher/profile as version inspection and
app-server. Outcome cleanup cannot be confirmed if any upstream helper's supervision fails.
Its raw-call failure remains retained; successful app-server exit cannot erase a helper failure.

The gateway checks the actual registered catalog, including the pinned parameter-schema projection, before sending. Native 0.167.2 omits
numeric minimum/maximum and string minLength/maxLength/pattern keywords; the host dispatcher enforces those bounds independently.
Codex 0.167.2 can register it at top level or in a Responses Lite `additional_tools` item under
the single `functions` namespace. The expected catalog consists of the selected navigation
tools and the unavoidable native `request_user_input`. Usage capture and tool validation assemble the same SSE
events, including multiline data and CRLF framing, before inspecting JSON; forbidden tool
calls cannot hide across data lines. Owner input is denied locally; forbidden native/custom
calls terminate the session without reaching a broker or human.

Each bounded response is fully drained before it is released to app-server. Terminal raw
HTTP/SSE usage and collector acknowledgement therefore precede tool replies and any subsequent
inference, even when tool events appear first. Responses, requests and total app-server stdout
are capped at 4 MiB; headers at 16 KiB. Cumulative upstream bodies are bounded at 16 MiB
per invocation (an overshooting call is retained and stops the session). Read/write and upstream deadlines share the invocation's
absolute deadline. Callbacks have the same deadline contract.

`RawCallRecord` has wire discriminator `af/codex-navigation-call/1` and strict schema
[`codex-navigation-call-v1.json`](../schemas/codex-navigation-call-v1.json). Each record binds
its unique send intent, exact request/response hashes, HTTP status, byte count, monotonic send
and drain observations, raw terminal observations and failure. It is passed by shared reference
to the native collector. The collector must make it immutable in the native Store. This module
does not write or replay a separate receipt ledger.

`RawField::Absent` and `RawField::Present(null)` serialize distinctly. Malformed values and
multiple terminal observations remain visible. Missing, late, duplicate, nonterminal or
contradictory usage stops further sends. App-server normalized token notifications are never
used as raw evidence. Inclusive usage is input plus output; chargeable usage is input minus
cached input plus output. Cache and reasoning counters are subsets, never added twice.
Inconsistent subsets, totals and overflow fail closed. Known raw usage remains available in
`Outcome.calls` even if later protocol processing, collector acknowledgement or cleanup fails.
GET `/models` is metadata, explicitly has no inference usage, and still passes the native guard.

`Outcome` records the monotonic invocation start and final drain. Successful supervision also
provides spawn, observed exit and drain times; failed supervision leaves those observations
unknown. Duplex supervision uses `af-process`. On macOS both version inspection and app-server
execution use `/usr/bin/sandbox-exec` with the exact inline profile
`(version 1) (allow default) (deny process-fork)`. The launcher must be a regular native file
with SHA-256 `58839ef01b4eef8aac0d2aa8f9d1c074ae45aafe3533965b030672450064acc8`;
unavailable or changed bytes fail before launch. The launcher path, digest and profile are
bound into `config_identity()` and therefore every native call intent. An OS update requires
renewed launcher admission and conformance, without a permissive fallback.

The kernel denies descendant creation before executing the pinned trusted transport; successful
supervised exit and pipe drain then confirm the lifecycle without assuming a process-group kill
finds escaped descendants. Failures keep cleanup unconfirmed. Linux requires successful
supervision and no recorded descendant-cleanup fallback. Other hosts fail explicitly before
launch. This policy constrains the trusted transport's lifecycle; source isolation still belongs
to the separate Docker broker. It is not a sandbox for arbitrary code.

## Credential-free conformance

Ordinary gates use deterministic Python stdio and local synthetic SSE fixtures:

```sh
cargo nextest run --workspace --locked --profile ci \
  -E 'binary_id(af-runner-codex) & test(/^navigation::/)'
```

The installed app-server test is ignored in ordinary gates. Run it explicitly with the native
0.167.2 executable; missing or incompatible prerequisites fail, rather than counting as a pass:

```sh
AF_NAVIGATION_CODEX=/absolute/path/to/native/codex \
  cargo nextest run --workspace --locked --profile ci --run-ignored only \
  -E 'binary_id(af-runner-codex) & test(/^navigation::tests::installed_app_server_conformance$/)'
```

It uses only committed nonfunctional dummy auth and a local HTTP fixture. No production URL,
personal credentials or paid model is used. The fixture verifies registration in both arms,
allowed dispatch, baseline semantic call refusal, local owner-input denial and raw collection order.
On supported macOS it requires a successful
answer, two raw calls, exit/drain timing and confirmed lifecycle cleanup under the pinned
launcher. The ordinary `macos_launcher_denies_fork_spawn_and_subprocess` test directly checks
EPERM from all three OS creation paths under the same launcher/profile. The ARM64 installed
check verifies `97bc82fd46f117cf10efb95e6ff0c81dc4784f5797bcdc2cb75093ba09e4def0`.
The Linux x86-64 check verifies `a9672d0fa0dd8fcf8cad136fa0d056b914bc046f5ac3d78f4a5cf390655af56b`.
The catalog records this manual prerequisite as `installed-conformance`, class e2e; an ignored
installed check is NOT RUN, independently of deterministic fixture passes.

The [external smoke plan](agent-bench.md#admit-an-external-smoke-run) uses the native Task Store
for reservations and settlement. The Store indexes each intent against the started Attempt
before releasing the request to the upstream. It indexes the raw response with the Attempt's
known usage floor before releasing the response to app-server. Offline replay also reads
intents and responses whose Attempt lost its writer before settlement.

Protocol references: [official app-server documentation](https://learn.chatgpt.com/docs/app-server)
and [official native-auth gateway mapping](https://learn.chatgpt.com/docs/enterprise/connect-to-a-gateway#enable-chatgpt-sign-in-recommended).

The credential-free transport-to-production-broker conformance (no installed Codex required)
is explicitly included with:

```sh
cargo nextest run --workspace --locked --profile ci --run-ignored all \
  -E 'binary_id(af-runner-codex) & test(/docker_broker_transport_unicode_and_pagination_conformance/)'
```

It uses the deterministic stdio peer, dummy auth and a local HTTP fixture with the real Docker
broker. It checks shared tool dispatch for Unicode multiline reads, binary-file reporting and
listing pagination across more than 1,024 paths. Separate installed CLI tests exercise native external smoke admission and settlement.
The [separate fixture grading Task](agent-bench.md#grade-retained-fixture-answers) uses the
same Store hooks with a fresh session for each admitted judgment.
