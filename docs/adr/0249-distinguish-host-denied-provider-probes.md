# ADR-0249: Distinguish host-denied Provider probes from incompatible CLIs

**Status:** accepted (2026-10-26). Refines [ADR-0146](0146-refuse-a-provider-cli-that-does-not-accept-afs-invocation.md).

## Context

Inside an agent sandbox, `codex app-server` can exit before answering `initialize` with
`Operation not permitted (os error 1)`. ADR-0146 classified every such exit as missing protocol
support and offered a CLI upgrade. That error establishes neither protocol capability nor login.

## Decision

A permission error from spawning a Provider capability probe, or from its stderr when it exits
before answering, is a `probe_denied` refusal. The error names the operation that failed and
advises retrying outside the sandbox or with host-approved permissions. A sandbox marker supplies
diagnostic context only. It cannot change a successful probe or demonstrated incompatibility, and
a version string cannot prove compatibility. Missing flags, a CLI that rejects the protocol, and
malformed protocol answers retain ADR-0146's incompatible classification and upgrade guidance.
Admission remains closed; no denied probe establishes login. Capability and detection probes read
no credentials and make no model call.

`af provider status` and `af provider detect` exit 8 for a denied probe, distinct from exit 4
for a missing or incompatible CLI. Their JSON documents are `af/provider-status@4` and
`af/provider-detect@4`, specified by `schemas/provider-status-v4.json` and
`schemas/provider-detect-v4.json`. Setup reports the same refusal as `probe_denied`, exit 8, in
`af/provider-setup@2` (`schemas/provider-setup-v2.json`), before asking for login. `af provider
add` also refuses with exit 8 before registration. `af doctor` adds harness, sandbox and network
diagnostic rows as `af/doctor@3` (`schemas/doctor-v3.json`) and does not offer `detect --apply`
when a Provider probe was denied. Earlier versions are not emitted or read (ADR-0145).

## Verification

Unit and CLI fixtures cover permission denial with and without sandbox markers, preserve the
existing missing-protocol fixture, validate the versioned documents, and assert that detection
does not read credentials or start a model turn.
