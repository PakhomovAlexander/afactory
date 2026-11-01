# ADR-0271: Own the Pi SDK host and confine its tools

Date: 2026-11-01. Status: proposed; accepted on merge.

## Context

Pi 1.4.0's stock CLI cannot establish AF's required stable account currentness, and its
built-in file tools can reach outside cwd. The proposed CLI/RPC Provider architecture in
the [Pi design](../design/pi-provider.md) therefore cannot preserve ADR-0090 by itself.
The official SDK exposes explicit auth storage, model runtime, resources, settings,
sessions and tools, allowing AF to own those boundaries inside one credential-bearing
process. Pi's canonical `openai-codex` OAuth credential includes non-secret `accountId`.

## Decision

Embed a small AF-owned Node ES-module host in `review-runner-pi`, behind a native Rust
`WorkerModelAdapter`. Require external Node >=22.19 and official Pi SDK exactly 1.4.0,
installed outside repository-owned paths. Never auto-install or execute a project module.
The host's versioned `af.pi-host/1` JSONL protocol serves token-free identity/catalog probes
and one isolated Attempt invocation under the common `review-process` supervisor.

Admit only explicitly registered `openai-codex` stored OAuth. The host uses Pi `AuthStorage`
and hashes only `accountId` with an explicit AF Pi/backend domain. Raw credentials and
account IDs stay inside that process. Inspect raw stored credentials via AuthStorage's locked
`modify` callback returning `undefined` (no write); never call its helper-capable `read`. A restricted credential store checks account identity
on every read and on both sides of a refresh callback, allowing token rotation and refusing
account replacement. The same store supplies inference. No context-only identity, credential
hashing, ambient API key, cloud auth or credential helper is admitted. This preserves
ADR-0090's identity rule while moving the native credential owner from a CLI to AF's SDK host.

Create `ModelRuntime` with no custom models or model networking, and `createAgentSession`
with explicit model/thinking, in-memory settings/session and an empty resource loader.
Disable agent/provider retries, compaction, cache warming and prompt expansion. No global
or project resources, extensions, MCP, context files or native session persistence apply.
Capture backend, auth method, opaque principal, exact SDK/executable/model/effort and
non-secret model/policy identities in versioned plan/admission authority. Recheck before
private input; a changed effective setting refuses rather than clamping or falling back.

Replace every exposed built-in file tool with AF-owned operations, under exact readable
and writable root grants from the Attempt's sandbox. Read-only Workers get read/grep/find/ls;
writable Workers also get write/edit. No shell or other executable tool is exposed. Absolute,
parent and symlink escapes refuse; an attached base is read-only. This is an exposed-tool
boundary under `trusted_local`, not a claim of OS isolation. Native execution remains
`trusted_unsafe`, with delivery/publication authority unchanged.

Require actual SDK settlement and final text; acknowledgements, `agent_end` and EOF are
insufficient. Observe native response usage so SDK-initialized zero counters cannot stand
for missing reports. Replace cumulative observations per inference, retain partial billing
floors on every failure and apply the ordinary local typed Worker contract and retry path.
All processes use the original Attempt wall, cancellation and common owned-group cleanup.

## Consequences

A usable supported SDK path replaces refusal-only Pi documentation. External Node/SDK
installation remains an operator dependency. Other backends and authentication modes remain
unsupported. Closed schema versions change together with writers, readers and parity tests;
old types and registry version 1 fail closed under ADR-0145.

Hermetic host/process fixtures exercise inference lifecycle without model tokens. A separate
token-free test uses the installed official SDK's real session/tool registry and synthetic
HTTP streams when available, without contacting a model service.
Neither is described as paid live smoke; that smoke was not run in the implementation sandbox.
