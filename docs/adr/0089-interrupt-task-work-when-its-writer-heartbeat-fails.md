# ADR-0089: Interrupt Task work when its writer heartbeat fails

Date: 2026-09-13
Status: Proposed

## Context

The native cancellation boundary in ADR-0087 does not stop work until its caller supplies a
control. A failed heartbeat can otherwise leave a Provider running until its original timeout,
even though its writer can no longer publish the result. Checking only the projected lease
expiry also misses a replacement writer whose lease extends far into the future.

## Decision

Give each common CLI execution a local cancellation flag shared by its TaskRuntime and
heartbeat. Cover ordinary Task execution, generated planning, Review and Provider doctor.
Add optional controlled host execution; existing callers without a control retain their
previous path. A host or adapter that cannot consume a supplied control refuses before work.
The control is local process state, not a new captured input, permission or spending allowance.

On each existing heartbeat tick, check the exact writer and epoch as well as lease time.
Use a read-only Store operation that grants no dispatch authority and appends no event.
Keep the one-second tick, renewal threshold and lease duration unchanged. A failed read,
renewal or heartbeat unwind requests cancellation before the owner waits for work to finish.
Ordinary heartbeat shutdown does not request cancellation.

Forward the control through Provider admission, Task Workers, captured Review, command
transports, Gates and Integration checks to their existing supervised execution boundaries.
Finite pure operations check the flag before work. Runtime admission and selection also check
it, so a successful output race cannot silently start another node or satisfy acceptance after
the host has observed interruption. Preserve every original context, model, credential policy,
Attempt identity, timeout, Task resource limit and generated-plan approval requirement.

The runtime still owns Broker accounting outside Worker invocation. Cancellation prevents later
Broker calls, and returned or late receipts retain their original accounting authority. It does
not turn an arbitrary synchronous connector into an interruptible transport; an installed
connector remains responsible for enforcing its captured deadline.

Retain reported usage and billing completeness in the existing sidecar before fallible
publication, as in ADR-0088. Lost writer authority cannot mint a replacement lease or permit
stale output admission. A real successor writer uses normal fenced recovery, preserves the
original budget and known charge floors, and cannot repeat a completed paid invocation.

## Considered options

- An adapter-only control leaves production CLI calls running after heartbeat failure.
- Comparing only lease expiry does not establish the current writer's identity.
- Silently ignoring a supplied control makes substitutable hosts behave differently.
- Cancelling accounting with execution loses paid observations needed by recovery.

## Consequences

The shared Task host now connects writer failure to the actual supervised process. Synthetic
CLI evidence can observe a live native leader and descendant, interrupt them through a failed
heartbeat, retain wide and incomplete usage, and reopen under the real successor writer without
another probe. Command, Gate, Integration and Broker controls cover the same forwarding boundary.
Exact checkpoint tests, full verification and CI remain separate recorded evidence.

This decision does not install CLI signal handlers, add a terminal domain-level cancelled
TaskResult, change SourceControl, or introduce a scheduler or ledger. Automatic mid-execution
replanning remains deferred by the product design; host interruption does not authorize a new
plan, approval, capability or resource extension.
