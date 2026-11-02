# Pi coding agent Provider

Status: implemented SDK host architecture, 2026-11-01. See proposed
[Pi SDK host decision](../adr/2026-11-02-own-the-pi-sdk-host-and-confine-its-tools.md).

AF supports **Node >=22.19** and the installed official
`@earendil-works/pi-coding-agent` **1.4.0** SDK, release
[`4a080252f06caf63a525d7873c5d94d3c23887e0`](https://github.com/earendil-works/pi/tree/4a080252f06caf63a525d7873c5d94d3c23887e0).
The stock Pi CLI's readiness response has no stable principal, and its built-in file tools
are not confined. AF therefore embeds its own ES-module host in `review-runner-pi`; it does
not attribute an invented identity protocol or filesystem boundary to the stock CLI.
No npm install, SDK update, model refresh or paid probe runs during registration or status.

## Supported matrix and setup

| Backend | Authentication | Support |
|---|---|---|
| `openai-codex` | Stored Pi OAuth with canonical nonempty `accountId` | Supported SDK host path; paid live smoke not run |
| `openai`, `anthropic`, any other backend | Any | Refused |
| Any | API key, environment/cloud auth, executable helper | Refused |
| Any | Custom endpoint, `models.json`, extensions, MCP, warm session | Refused |

Install Node and the official SDK outside a repository. Complete Pi's OAuth login at a
private terminal, then register that existing private profile:

```sh
af provider setup pi-personal --kind pi --backend openai-codex \
  --auth-dir /absolute/private/pi-profile \
  --sdk-entrypoint /installed/node_modules/@earendil-works/pi-coding-agent/dist/index.js \
  --json
af provider status --json
af provider models pi-personal --json
```

`--sdk-entrypoint` may be omitted when the installed Pi launcher safely resolves to its
SDK `dist/index.js`. For the standard managed `<agent>/bin/pi` launcher, AF reads
`<agent>/install/current-version` as exactly supported version `1.4.0` and resolves
`install/releases/1.4.0/node_modules/@earendil-works/pi-coding-agent/dist/index.js`.
It never executes the launcher. Malformed/unsupported versions, symlinked version files,
release/entrypoint escapes and repository-owned installations refuse. Explicit entrypoints
and safely resolved SDK CLI layouts remain supported. Both Node and the SDK must resolve
outside repository-owned paths: every ancestor is checked for `.git` (directory or worktree
marker file) and `.af`, even when CWD is nested and PATH names an absolute external directory.
A package-manager prefix containing `.git` also refuses; AF does not exempt ignored installation
subdirectories from this path boundary. `NODE_OPTIONS`, `NODE_PATH`, proxy,
cloud and API-key environment variables are absent from the host. `--login` and
`af provider auth` are unavailable for Pi; login stays human-driven in Pi's own terminal.
`provider detect` scans the existing Codex/Claude contexts; Pi uses explicit setup.

The machine-local registry is version **2**, retaining the auth-directory uniqueness rule.
Pi entries require `kind = "pi"`, `backend = "openai-codex"`, `auth_dir` and
`sdk_entrypoint`. Version 1 is refused; recreate registrations with this release rather
than migrating an older registry (ADR-0145). Existing Codex and Claude entries use the
same fields in version 2, and their adapters and default assignments are unchanged.
A Pi setup result is `af/pi-provider-setup@2`; status is `af/provider-status@5` and
model discovery is `af/provider-models@2`. Setup emits that Pi report for every internal
refusal after Pi setup is invoked, including discovery, invalid configuration, installation,
authentication and registry failures. In setup reports, `id` is null for an invalid/reserved
requested ID and `backend` is null for an absent/unsupported requested backend; AF never
invents a supported identity or echoes invalid raw request values. Registered/conflict outcomes
require both validated values. Failures print the same safe AF-authored detail and outcome on
stderr, including with `--json`; early refusals exit 3 and registry conflicts exit 5.
CLI parser errors before invocation remain ordinary usage errors. Setup report v1 and host
protocol v1 fail closed under ADR-0145. Status proves authentication, never paid
usability. Subscription quota probing is unsupported.

## Account, model and captured authority

The host alone opens Pi `AuthStorage` for the explicit profile's `auth.json`. It accepts
only a canonical stored OAuth credential and hashes **only** its non-secret `accountId`,
with domain `af.pi.account/openai-codex\0`. Access and refresh tokens never leave that
process or become artifacts, diagnostics or a principal hash. Registrations for the same
account produce the same opaque principal, so aliases cannot establish independence.

The host uses AuthStorage's locked `modify` callback returning `undefined` (no write) to
inspect raw credentials: `AuthStorage.read` can execute API-key helpers before returning,
so AF never calls that helper-capable method. The restricted credential store validates
the expected account on **every** read and inside **every** refresh transaction, before and
after the SDK's update callback. The host also compares `accountId` to the non-secret routing
account claim inside the access token, which Pi's inference adapter uses, so a stale field cannot conceal token replacement.
Only that account identifier is hashed; the token and other claims stay inside the host. Token
rotation for that account remains supported. Missing identity, an API key, or account
replacement fails closed. The same guarded store is supplied to `ModelRuntime`; no
environment or helper can supply another inference credential.

Workers keep `--provider NODE=ID`. Their `provider_kind` is `pi`; their `model` is the
exact SDK backend model ID, and `effort` is a supported Pi thinking level. Pi accepts
`off minimal low medium high xhigh max`, narrowed by the selected model's catalog.
The SDK's model-specific clamping is checked after session construction: AF refuses a
changed effective setting. Pi does not apply the Codex/Claude hyphen-and-digit heuristic;
unknown IDs and unsupported efforts refuse before private prompt delivery.

`WorkerExecutionV2::Model.pi` captures backend, OAuth method, exact SDK version, SDK and
Node paths, auth-context identity, static model-configuration digest (including endpoint)
and embedded host-policy digest. The opaque principal, model and effort remain separate
fields. Changed configuration requires normal plan revision/admission. Changed contracts
are `af/ExecutionPlan@2`, `af/TaskProviderAdmission@2`,
`af/LegacyReviewTaskPolicy@5`, `af/LegacyReviewInvocationPolicy@2` and
`af/provider-doctor@3` and `af/task-inspection@15`, with closed schemas and parity tests. Unsupported prior types
are not readers or fallback paths.

## SDK construction and exposed tools

The host uses `ModelRuntime.create` with the guarded credential store, `modelsPath: null`,
`allowModelNetwork: false`, and `refreshOnCreate: false`. `PI_OFFLINE=1` also disables
native catalog networking. It uses `createAgentSession` with the explicit model/runtime,
`SessionManager.inMemory(cwd)`, `SettingsManager.inMemory`, an empty `ResourceLoader`,
`noTools: "all"`, an exact tools allowlist and AF tool definitions that replace built-ins.
No project/global settings, packages, context files, skills, themes, prompt templates,
extensions or MCP resources are discovered. Prompt-template/command expansion is disabled.

Read-only Workers receive `read`, `grep`, `find`, `ls`. Writable Workers also receive
`write`, `edit`. No bash, PowerShell, arbitrary tool, subagent or codemode is exposed,
including for a Worker whose effects permit checks. Such a Worker must use a separate
check node for commands. Only the materialized source and explicitly attached base are
readable; only a writable source is writable. Relative or absolute paths outside those
roots, symlink traversal and writes into the base refuse. Parent spelling can reach an
explicitly granted base, never an arbitrary parent directory. File opens use `O_NOFOLLOW`
and file-type checks; traversal, file sizes and tool results are bounded. Grep and find
use literal substrings, with no external process or regular-expression engine.
Write creates missing parent directories only inside its writable root, checking every component.

This confines the actual model-visible operations; it does **not** turn `trusted_local`
into OS isolation against unrelated host processes or a compromised installed SDK.
Native credential-bearing execution remains `trusted_unsafe`. No delivery, publication,
remote gate or `auto_apply` authority is granted.

## Protocol, lifecycle and accounting

`af.pi-host/2` is AF's versioned JSONL protocol, with schema `schemas/pi-host-v2.json`:

1. Rust sends `init`, containing no private Worker prompt. The host returns `ready` with
   safe opaque identity and a bounded static catalog. A token-free probe ends here.
2. Rust compares captured identity/configuration, then sends `prepare` with exact model,
   effort and model digest. Only after `prepared` matches does Rust send `prompt`.
3. Rust generates a fresh 256-bit random `prompt_nonce` challenge in the prompt writer and
   includes it only in the delivered `prompt` record. The host rechecks the guarded credential
   and echoes that exact challenge in `accepted` before inference. Rust requires this echo
   before accepting usage, messages or settlement: a speculative reply after `prepared`
   cannot answer a prompt the host has not consumed. The expected challenge is published
   before writing so an immediate legitimate reply cannot race the writer's completion.
   A failed or partial prompt write retains unknown usage conservatively. This acknowledgement
   is never a result. One SDK session runs in this one process.
4. Usage observations name an inference sequence; cumulative snapshots replace that
   inference's earlier observation. Completed duplicates do not count twice. Successful
   settlement requires at least one sequentially numbered observed inference and a final
   `complete: true` snapshot for every observed inference. Missing or unfinished observations
   refuse the reply while retaining partial/unknown accounting. Individual counters may stay
   unknown even in a complete snapshot; lifecycle completion does not invent usage values.
5. The host requires actual `agent_settled`, successful final assistant text and the return
   of `session.prompt`. `agent_end`, EOF and exit zero cannot establish success.

LF alone frames records; CRLF is accepted and U+2028/U+2029 remain string content. Partial
records, incompatible versions, invalid state transitions, changed identities/settings,
trailing events after settlement, oversized output and native errors refuse. Only typed
safe error codes cross the host boundary; stderr and raw credential responses never become
Worker evidence. `review-process` supervises the bidirectional pipes, original Attempt
deadline, cancellation and owned process group; no adapter-owned process manager exists.
A protocol/output failure also signals that supervisor. Already observed usage survives
cancellation, EOF, errors, invalid output and transport failure.

Agent retry and compaction are disabled; provider HTTP retries are zero, transport is SSE,
and cache warming is off. No auxiliary inference, session persistence or automatic restart
is enabled. Retries for malformed typed output remain the existing common Task handling,
with the first call's spend retained. The local Worker schema still validates results;
Pi is not assumed to provide native constrained JSON generation.

Pi initializes its assistant counters to zero even when the server has not reported usage.
AF instead observes native response events through `onProviderStreamEvent`:

| AF counter | Native field | Treatment |
|---|---|---|
| input | `response.usage.input_tokens` | Total, includes cache components |
| output | `response.usage.output_tokens` | Includes reasoning |
| cache read | `input_tokens_details.cached_tokens` | Separate component, excluded from charge |
| cache write | `input_tokens_details.cache_write_tokens` | Separate component, already in input |
| reasoning | `output_tokens_details.reasoning_tokens` | Separate component, already in output |
| charge | input minus cache read plus output | Each inference once |

Missing counters stay unknown. A partial report retains its known billing floor; a complete
charge requires known input, output and cache read for every inference and successful
settlement. Cache write and reasoning may remain unknown without inventing zero or double
charging them. Monetary estimates are not invoices. The Attempt's reservation and deadline
bound the whole invocation; AF promises no unimplemented provider-side hard token cap.

## Evidence and remaining live smoke

`scripts/test-pi-host.py` instantiates the installed **real** SDK without real credentials or a
service call when available. It exercises the session's actual wrapped AF tools against
synthetic absolute/parent/symlink markers, read-only/writable/base permissions, cancellation,
empty resources, disabled retries/compaction and account rotation/replacement. CI runs the
hermetic SDK-signature equivalent when the external SDK is absent. With the official SDK,
the host also consumes synthetic SSE responses through its real provider adapter, dispatches a
confined read and nested write through the SDK's tool-parameter validator, returns a structured
`af.worker-reply/1`, observes two inferences with native input/output/cache/reasoning counters
and reaches actual session settlement, with HTTP replaced in process and no service call.
The Rust runner tests use
faithful **AF host** processes for JSONL fragmentation, Unicode, terminal ordering, identity
and setting drift, limits, malformed events and cancellation with preserved usage/reaping.
A bounded manual transport regression captures the complete early response before delivering
any prompt, then delivers the exact private prompt, asserts its read marker and observes the
child's successful exit. Replaying those ordered wire records through the production state
validator refuses the premature acknowledgement. The adapter-level refusal and immediate
valid-response tests remain separate; production cancellation is unchanged.
CLI fixtures exercise setup/status/catalog, common Task admission and typed-result recovery,
plus doctor and captured Review. Existing-provider regression selections remain required.

Run `python3 -I scripts/test-pi-host.py` for portable discovery of the pinned installation
under `~/.pi/agent/install/releases/1.4.0` or beside a resolved `pi` launcher. Discovery reads
paths only; it never executes the launcher or npm. An explicit
`AF_PI_TEST_SDK=/installed/node_modules/@earendil-works/pi-coding-agent/dist/index.js`
selects a trusted external installation; a missing or repository-owned explicit path fails.
`AF_PI_TEST_HERMETIC=1` selects the CI fixture without loading an installed SDK. Both modes
print whether official SDK construction and full agent-turn coverage actually ran; an absent
SDK leaves that coverage explicitly **not run**, while the hermetic host protocol tests run.

**Paid live smoke: not run** for `openai-codex` OAuth. No actual login, OAuth refresh exchange,
quota request or paid inference was used for implementation evidence. The successful
official SDK test proves construction, tool confinement and synthetic stream handling; the
synthetic inference fixtures prove AF's protocol/runtime behavior. They are not a live service receipt.

The [indexed validation record](pi-provider-validation.md) retains the original issue #494
observation and historical measurement runs with their historical file bindings. A
reproducible source-content projection separately binds the fresh manual measurement and current
checks without including the validation record itself. No historical execution is reconstructed
or attributed to this candidate.
