# 0072 — Retain process output independently of transport status

Status: accepted. Date: 2026-09-12.

## Context

A native Task Worker may report usage and then fail during input delivery, output draining or
CAS publication. The shared supervisor retained timeout bytes, but other error paths discarded
already-read prefixes. Losing those bytes could also lose reported usage.

## Decision

The dependency-neutral process supervisor exposes an additive captured result: status, stdout,
stderr and the existing held-stderr flag. Existing buffered and streaming `Result` APIs are
compatibility wrappers over the same execution core. They preserve their error variants,
including typed streaming-input failures and the existing timeout payload.

Each pipe drain accumulates bounded-size read chunks into one shared byte buffer. It does not
hold a lock across blocking reads. Failure cleanup can retain the prefix even when the reader
fails or a descendant holds the pipe. Wait/deadline errors take precedence over stdin, stdout
and stderr errors. Process-group termination and existing deadline/grace rules remain shared.
Duplex protocol readers retain their streaming interface; their complete stdout is not buffered.

Model capture redacts both retained streams on every path. Task adapters parse reported usage
before refusing a failed message. CAS publication failure still preserves the in-memory bytes.
No failed transport is promoted to successful Worker output, and no absent usage is invented.

## Alternatives

Adapter-specific supervisors would duplicate the process boundary from ADR-0026. Adding captured
bytes only to error formatting would mix diagnostics with provider protocol and miss read-error
prefixes. Treating held stdout as timeout would change established error and retry semantics.

## Verification

Deterministic readers retain bytes before a read failure. Process and model fixtures cover typed
stdin failure, held stdout, redaction and unchanged failure classification. Both native Task
adapters retain an exact `u64::MAX` usage report while refusing held-output and timeout/CAS
failures. The 500 ms timeout fixture prepares its newly written executable through a bounded,
empty readiness branch before the measured invocation; its deadline and retention assertions
remain unchanged. First-execution delay was reproduced before the script's first instruction.
